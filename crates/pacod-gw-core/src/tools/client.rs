//! The gateway as an MCP client of a tool server, over Streamable HTTP, in both revisions.
//!
//! The 2026-07-28 revision is stateless: every request carries its protocol version in `_meta`
//! and in the `MCP-Protocol-Version` header, and `Mcp-Method` and `Mcp-Name` repeat the method
//! and the tool. The 2025-11-25 revision opens a session with `initialize` and may give it an
//! id (`Mcp-Session-Id`) that later requests carry. The client tries the newer revision first
//! and falls back to `initialize` when the server does not know it, as the 2026-07-28 revision
//! tells clients to (its "Backward compatibility" section).
//!
//! Tool servers are named by organisation administrators, so every connection is checked
//! against the tool servers' egress policy (`PGW_TOOL_EGRESS_ALLOW_PRIVATE`), after the name is
//! resolved, and a redirect is an error rather than followed: the policy is not asked twice.
//! An answer is read up to a size limit, as JSON or as a stream of server-sent events; a
//! server that asks the caller for input (sampling, elicitation) is refused, since the gateway
//! passes none of that on (catalogue MCP-05).

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use pacod_gw_events::egress::EgressPolicy;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};

use crate::catalog::fetch::GuardedResolver;

/// The current revision, which the client speaks first.
pub const MODERN: &str = "2026-07-28";
/// The revision the client asks for in `initialize`.
pub const LEGACY: &str = "2025-11-25";
/// The earlier revisions a server may agree to in `initialize`: their tools are alike.
const LEGACY_REVISIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
/// Pages of a tool list followed at most.
const MAX_PAGES: usize = 20;
/// What is read of an answer that is an HTTP error.
const MAX_ERROR_BYTES: usize = 64 << 10;
/// JSON-RPC codes of the 2026-07-28 revision that a server of that revision answers with.
const UNSUPPORTED_VERSION: i64 = -32022;
const HEADER_MISMATCH: i64 = -32020;

/// The client, with its pooled connections. Cheap to clone.
#[derive(Clone)]
pub struct ToolClient {
    http: reqwest::Client,
}

/// Where and how to reach one tool server.
pub struct Target<'a> {
    pub url: &'a str,
    /// The credential's header and value; `None` sends none.
    pub auth: Option<(&'a str, String)>,
    pub timeout: Duration,
    /// The largest answer read.
    pub max_bytes: usize,
}

/// How a request reaches the server: statelessly, or within a session of the 2025-11-25
/// revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Modern,
    Legacy(Session),
}

impl Link {
    /// The revision the server speaks, for records.
    pub fn version(&self) -> &str {
        match self {
            Self::Modern => MODERN,
            Self::Legacy(s) => &s.version,
        }
    }
}

/// A session of a server of the 2025-11-25 revision (or an earlier one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The revision the server agreed to.
    pub version: String,
    /// The session's id, when the server gave one.
    pub id: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("the tool server could not be reached: {0}")]
    Unreachable(String),
    #[error("the tool server did not answer within {0:?}")]
    Timeout(Duration),
    #[error("the tool server answered HTTP {0}")]
    Status(u16, Value),
    #[error("the tool server's answer is larger than {0} bytes")]
    TooLarge(usize),
    #[error("the tool server's answer is not an MCP answer: {0}")]
    Malformed(String),
    #[error("the tool server asks for input from the caller, which the gateway does not pass on")]
    InputRequired,
    #[error("the tool server no longer knows the session")]
    SessionGone,
}

impl ClientError {
    /// The kind of failure, for records and metrics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Unreachable(_) => "unavailable",
            Self::Timeout(_) => "timeout",
            Self::Status(..) => "upstream_error",
            Self::TooLarge(_) => "too_large",
            Self::Malformed(_) => "invalid_response",
            Self::InputRequired => "input_required",
            Self::SessionGone => "session_gone",
        }
    }
}

/// One answer: the JSON-RPC response, its size, and the session id the server gave with it.
#[derive(Debug, Clone)]
pub struct Answer {
    pub message: Value,
    pub bytes: usize,
    pub session_id: Option<String>,
}

impl ToolClient {
    pub fn new(policy: Arc<EgressPolicy>) -> reqwest::Result<Self> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .dns_resolver(Arc::new(GuardedResolver(policy)))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("pacod-gw/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { http })
    }

    /// Sends one JSON-RPC request (or, without an `id`, a notification) and reads its answer.
    pub async fn call(
        &self,
        target: &Target<'_>,
        link: &Link,
        id: Option<u64>,
        method: &str,
        params: Value,
    ) -> Result<Answer, ClientError> {
        let mut body = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        if let Some(id) = id {
            body["id"] = json!(id);
        }
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json, text/event-stream"));
        headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if let Some((name, value)) = &target.auth {
            let name = HeaderName::from_bytes(name.as_bytes()).map_err(|e| ClientError::Malformed(e.to_string()))?;
            let mut value = HeaderValue::from_str(value).map_err(|_| ClientError::Malformed("credential".into()))?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        match link {
            Link::Modern => {
                let meta = body["params"]["_meta"].as_object().cloned().unwrap_or_default();
                let mut meta = Value::Object(meta);
                meta["io.modelcontextprotocol/protocolVersion"] = json!(MODERN);
                meta["io.modelcontextprotocol/clientCapabilities"] = json!({});
                meta["io.modelcontextprotocol/clientInfo"] =
                    json!({ "name": "pacod-gw", "version": env!("CARGO_PKG_VERSION") });
                body["params"]["_meta"] = meta;
                headers.insert("mcp-protocol-version", HeaderValue::from_static(MODERN));
                headers.insert("mcp-method", header_value(method));
                let name = match method {
                    "tools/call" | "prompts/get" => body["params"]["name"].as_str(),
                    "resources/read" => body["params"]["uri"].as_str(),
                    _ => None,
                };
                if let Some(name) = name {
                    headers.insert("mcp-name", header_value(name));
                }
            }
            Link::Legacy(session) => {
                if method != "initialize" {
                    if let Ok(v) = HeaderValue::from_str(&session.version) {
                        headers.insert("mcp-protocol-version", v);
                    }
                    if let Some(sid) = session.id.as_deref().and_then(|s| HeaderValue::from_str(s).ok()) {
                        headers.insert("mcp-session-id", sid);
                    }
                }
            }
        }
        let exchange = async {
            let resp = self.http.post(target.url).headers(headers).json(&body).send().await.map_err(|e| {
                if e.is_timeout() { ClientError::Timeout(target.timeout) } else { ClientError::Unreachable(chain(&e)) }
            })?;
            read_answer(resp, link, id, target.max_bytes).await
        };
        tokio::time::timeout(target.timeout, exchange).await.map_err(|_| ClientError::Timeout(target.timeout))?
    }

    /// Opens a session with a server of the 2025-11-25 revision (or an earlier one).
    pub async fn initialize(&self, target: &Target<'_>) -> Result<Session, ClientError> {
        let opening = Link::Legacy(Session { version: LEGACY.into(), id: None });
        let params = json!({
            "protocolVersion": LEGACY,
            "capabilities": {},
            "clientInfo": { "name": "pacod-gw", "version": env!("CARGO_PKG_VERSION") }
        });
        let answer = self.call(target, &opening, Some(0), "initialize", params).await?;
        if let Some(error) = answer.message.get("error") {
            return Err(ClientError::Malformed(format!("initialize was refused: {error}")));
        }
        let version = answer.message["result"]["protocolVersion"].as_str().unwrap_or_default();
        if !LEGACY_REVISIONS.contains(&version) {
            return Err(ClientError::Malformed(format!("the server agreed to the revision {version:?}")));
        }
        let session = Session { version: version.to_owned(), id: answer.session_id };
        self.call(target, &Link::Legacy(session.clone()), None, "notifications/initialized", json!({})).await?;
        Ok(session)
    }

    /// Finds which revision the server speaks: the current one if it answers it, else a
    /// session of the earlier one.
    pub async fn connect(&self, target: &Target<'_>) -> Result<Link, ClientError> {
        match self.call(target, &Link::Modern, Some(0), "server/discover", json!({})).await {
            Ok(answer) if answer.message.get("result").is_some() => return Ok(Link::Modern),
            Ok(answer) => modern_or_not(&answer.message)?,
            Err(ClientError::Status(_, body)) => modern_or_not(&body)?,
            Err(e @ (ClientError::Unreachable(_) | ClientError::Timeout(_) | ClientError::TooLarge(_))) => {
                return Err(e);
            }
            // Anything else from a server of the earlier revision, which reads the request as
            // one without a session.
            Err(_) => {}
        }
        Ok(Link::Legacy(self.initialize(target).await?))
    }

    /// Every tool the server lists, following its pages, at most `max_tools`.
    pub async fn list_tools(
        &self,
        target: &Target<'_>,
        link: &Link,
        max_tools: usize,
    ) -> Result<Vec<Value>, ClientError> {
        let mut tools = Vec::new();
        let mut cursor: Option<String> = None;
        for page in 0..MAX_PAGES {
            let params = cursor.as_ref().map_or_else(|| json!({}), |c| json!({ "cursor": c }));
            let answer = self.call(target, link, Some(page as u64 + 1), "tools/list", params).await?;
            if let Some(error) = answer.message.get("error") {
                return Err(ClientError::Malformed(format!("tools/list was refused: {error}")));
            }
            let result = &answer.message["result"];
            let listed = result["tools"].as_array().ok_or_else(|| ClientError::Malformed("no tools".into()))?;
            tools.extend(listed.iter().cloned());
            if tools.len() > max_tools {
                return Err(ClientError::Malformed(format!("the server lists more than {max_tools} tools")));
            }
            match result["nextCursor"].as_str().filter(|c| !c.is_empty()) {
                Some(c) => cursor = Some(c.to_owned()),
                None => return Ok(tools),
            }
        }
        Err(ClientError::Malformed(format!("the tool list has more than {MAX_PAGES} pages")))
    }
}

/// Whether an error answer comes from a server of the current revision that cannot serve the
/// client: then falling back would not help. `Ok` means: try the earlier revision.
fn modern_or_not(message: &Value) -> Result<(), ClientError> {
    match message["error"]["code"].as_i64() {
        Some(UNSUPPORTED_VERSION) => {
            let supported: Vec<&str> = message["error"]["data"]["supported"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            if supported.iter().any(|v| LEGACY_REVISIONS.contains(v)) {
                Ok(())
            } else {
                Err(ClientError::Malformed(format!("the server speaks none of the revisions it knows: {supported:?}")))
            }
        }
        Some(HEADER_MISMATCH) => Err(ClientError::Malformed("the server read the headers otherwise".into())),
        _ => Ok(()),
    }
}

/// A header value for a method or a name: as it is when it is plain ASCII, else in the form the
/// 2026-07-28 revision gives for other text (`=?base64?...?=`).
fn header_value(value: &str) -> HeaderValue {
    let plain = value.bytes().all(|b| (0x20..0x7f).contains(&b)) && value.trim() == value;
    let text = if plain {
        value.to_owned()
    } else {
        format!("=?base64?{}?=", base64::engine::general_purpose::STANDARD.encode(value))
    };
    HeaderValue::from_str(&text).unwrap_or_else(|_| HeaderValue::from_static(""))
}

fn chain(e: &dyn std::error::Error) -> String {
    let mut text = e.to_string();
    let mut source = e.source();
    while let Some(s) = source {
        text.push_str(": ");
        text.push_str(&s.to_string());
        source = s.source();
    }
    text
}

/// Reads the answer to request `id` from a response: a JSON body, or the event of a stream that
/// carries it. Notifications in a stream are skipped; a request from the server ends the call.
async fn read_answer(
    mut resp: reqwest::Response,
    link: &Link,
    id: Option<u64>,
    max_bytes: usize,
) -> Result<Answer, ClientError> {
    let status = resp.status();
    let session_id = resp.headers().get("mcp-session-id").and_then(|v| v.to_str().ok()).map(str::to_owned);
    if status.as_u16() == 404 && matches!(link, Link::Legacy(Session { id: Some(_), .. })) {
        return Err(ClientError::SessionGone);
    }
    if !status.is_success() {
        let body = read_body(&mut resp, MAX_ERROR_BYTES).await.unwrap_or_default();
        return Err(ClientError::Status(status.as_u16(), serde_json::from_slice(&body).unwrap_or_default()));
    }
    if id.is_none() {
        return Ok(Answer { message: Value::Null, bytes: 0, session_id });
    }
    let stream = resp
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/event-stream"));
    let body = read_body(&mut resp, max_bytes).await?;
    let message = if stream {
        answer_in_stream(&body, id)?
    } else {
        serde_json::from_slice::<Value>(&body).map_err(|e| ClientError::Malformed(e.to_string()))?
    };
    if message.get("method").is_some() || message["result"]["resultType"] == "input_required" {
        return Err(ClientError::InputRequired);
    }
    if message.get("result").is_none() && message.get("error").is_none() {
        return Err(ClientError::Malformed("neither a result nor an error".into()));
    }
    Ok(Answer { message, bytes: body.len(), session_id })
}

async fn read_body(resp: &mut reqwest::Response, max_bytes: usize) -> Result<Vec<u8>, ClientError> {
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| ClientError::Unreachable(chain(&e)))? {
        if body.len() + chunk.len() > max_bytes {
            return Err(ClientError::TooLarge(max_bytes));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

/// The message of a stream of server-sent events that answers `id`, or the first request the
/// server makes of the client.
fn answer_in_stream(body: &[u8], id: Option<u64>) -> Result<Value, ClientError> {
    let text = std::str::from_utf8(body).map_err(|_| ClientError::Malformed("the stream is not text".into()))?;
    for event in text.replace("\r\n", "\n").split("\n\n") {
        let data: Vec<&str> =
            event.lines().filter_map(|l| l.strip_prefix("data:")).map(|d| d.strip_prefix(' ').unwrap_or(d)).collect();
        if data.is_empty() {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&data.join("\n")) else { continue };
        let is_request = message.get("method").is_some() && message.get("id").is_some();
        let answers = message.get("id").and_then(Value::as_u64) == id && message.get("method").is_none();
        if is_request || answers {
            return Ok(message);
        }
    }
    Err(ClientError::Malformed("the stream ended without an answer".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_answer_is_found_among_the_events_of_a_stream() {
        let stream = "event: message\ndata: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\",\"params\":{}}\n\n\
                      data: {\"jsonrpc\":\"2.0\",\"id\":7,\n\
                      data: \"result\":{\"content\":[]}}\n\n";
        let answer = answer_in_stream(stream.as_bytes(), Some(7)).unwrap();
        assert_eq!(answer["result"]["content"], json!([]));
        assert!(answer_in_stream(b"data: {\"jsonrpc\":\"2.0\",\"id\":8,\"result\":{}}\n\n", Some(7)).is_err());
        // A request of the server's own comes back for the caller to refuse.
        let asks = b"data: {\"jsonrpc\":\"2.0\",\"id\":\"s1\",\"method\":\"sampling/createMessage\"}\n\n";
        assert!(answer_in_stream(asks, Some(7)).unwrap().get("method").is_some());
    }

    #[test]
    fn a_name_that_is_not_plain_ascii_is_sent_in_base64() {
        assert_eq!(header_value("tools/call"), "tools/call");
        assert_eq!(header_value("café"), "=?base64?Y2Fmw6k=?=");
        assert_eq!(header_value(" padded"), "=?base64?IHBhZGRlZA==?=");
    }

    #[test]
    fn only_a_current_server_that_knows_no_earlier_revision_stops_the_fall_back() {
        let unsupported = |v: &[&str]| json!({ "error": { "code": -32022, "data": { "supported": v } } });
        assert!(modern_or_not(&unsupported(&["2025-11-25"])).is_ok());
        assert!(modern_or_not(&unsupported(&["2027-01-01"])).is_err());
        assert!(modern_or_not(&json!({ "error": { "code": -32601 } })).is_ok());
        assert!(modern_or_not(&json!(null)).is_ok());
    }
}
