//! `/mcp` and `/mcp/{bundle}`: the gateway's MCP server (catalogue ING-13, MCP-01, MCP-02,
//! MCP-04, MCP-13, MCP-14; runbook section 21).
//!
//! It serves the approved tools of the organisation's tool servers that the caller's key may
//! use (its bundles; `/mcp/{bundle}` one of them), each under the name `<server>__<tool>`, and
//! pacod-broker's discovery tools where the gateway has a broker (`broker.rs`). A call reaches
//! the tool server with the organisation's credential; the caller's key never leaves the
//! gateway.
//!
//! Both revisions are served on one endpoint, as the 2026-07-28 revision allows: a request with
//! its `_meta` is stateless and checked as that revision says (its headers against its body);
//! an `initialize`, or a request without that `_meta`, is of the 2025-11-25 revision, whose
//! `initialize` the gateway answers itself without a session id. No session is needed on any
//! instance, which is how catalogue MCP-04 is met. Only `POST` is served: axum answers any
//! other method with `405` and an `Allow` header, as both revisions ask of a server that offers
//! no stream of its own.
//!
//! The data plane's router turns axum's body limit off, so that each handler bounds what it
//! reads. This one reads the body only once the key is known, and no more than
//! `max_body_bytes`.
//!
//! Every `tools/call` leaves a record (`pacod_gw_events::ToolCallRecord`): who called which
//! tool, a keyed hash of the arguments, what the gateway decided and what came of it, also
//! when the caller goes away before the answer. The handshake and the list leave none; they
//! are counted in the metrics.

mod broker;
mod server;

use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{Path, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use http_body_util::BodyExt;
use pacod_gw_events::ToolCallRecord;
use pacod_gw_events::tool::protocol;
use serde_json::{Value, json};

use super::Gw;
use super::auth::{Caller, identify};
use super::client_ip::ClientIp;
use super::common::GatewayParams;
use super::errors::Envelope;
use crate::crypto::digest::arguments_hmac;
use crate::tools::client::{LEGACY, MODERN};

/// JSON-RPC's own error codes, and those of the 2026-07-28 revision.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;
const HEADER_MISMATCH: i64 = -32020;
const UNSUPPORTED_VERSION: i64 = -32022;

/// The revisions of the 2025 line a client may ask for in `initialize`; their tools are alike.
const LEGACY_REVISIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
/// Longest protocol version kept on a record: the revisions are dates.
const MAX_VERSION_CHARS: usize = 32;

/// The revision a request speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Era {
    /// 2026-07-28: stateless, every request with its `_meta`.
    Modern,
    /// 2025-11-25 and before: a session opened with `initialize`.
    Legacy,
}

fn server_info() -> Value {
    json!({ "name": "pacod-gw", "version": env!("CARGO_PKG_VERSION") })
}

/// A JSON answer and its size, for the record.
fn reply(status: StatusCode, body: Value) -> (Response, usize) {
    let bytes = body.to_string().len();
    ((status, Json(body)).into_response(), bytes)
}

/// A result. The 2026-07-28 revision marks every result as complete and names the server in its
/// `_meta`.
fn rpc_result(era: Era, id: &Value, mut result: Value) -> (Response, usize) {
    if era == Era::Modern && result.is_object() {
        result["resultType"] = json!("complete");
        result["_meta"]["io.modelcontextprotocol/serverInfo"] = server_info();
    }
    reply(StatusCode::OK, json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// An error answer. The 2025-11-25 revision carries errors with HTTP 200; the 2026-07-28
/// revision answers a request it refuses as a request with `400` (or `404` for an unknown
/// method), and a failure while serving it with 200, as `status` says.
fn rpc_error(
    era: Era,
    id: &Value,
    code: i64,
    message: &str,
    data: Option<Value>,
    status: StatusCode,
) -> (Response, usize) {
    let mut error = json!({ "code": code, "message": message });
    if let Some(d) = data {
        error["data"] = d;
    }
    let status = if era == Era::Modern { status } else { StatusCode::OK };
    reply(status, json!({ "jsonrpc": "2.0", "id": id, "error": error }))
}

/// The one JSON-RPC message of a request. Batches left the protocol in its 2025-06-18 revision,
/// and a gateway that took them would have to judge and record each member apart.
///
/// The error is a JSON-RPC code and its reason; the answer to it has no `id` to repeat.
fn message(bytes: &[u8]) -> Result<Value, (i64, &'static str)> {
    match serde_json::from_slice::<Value>(bytes) {
        Err(_) => Err((PARSE_ERROR, "not JSON")),
        Ok(Value::Array(_)) => Err((INVALID_REQUEST, "batches are not accepted: send one message per request")),
        Ok(v) if !v.is_object() => Err((INVALID_REQUEST, "not a JSON-RPC message")),
        Ok(v) => Ok(v),
    }
}

fn header<'h>(headers: &'h HeaderMap, name: &str) -> Option<&'h str> {
    headers.get(name).and_then(|v| v.to_str().ok())
}

/// The revision of a request: one that carries the 2026-07-28 revision's `_meta`, or names a
/// version of no earlier revision, is of that one; an `initialize` and everything else is of
/// the 2025 line.
fn era_of(headers: &HeaderMap, req: &Value) -> Era {
    if req["method"] == "initialize" {
        return Era::Legacy;
    }
    let meta = req["params"]["_meta"].get("io.modelcontextprotocol/protocolVersion").is_some();
    let named = header(headers, "mcp-protocol-version").is_some_and(|v| !LEGACY_REVISIONS.contains(&v));
    if meta || named { Era::Modern } else { Era::Legacy }
}

/// A header value as the 2026-07-28 revision writes text that is not plain ASCII
/// (`=?base64?...?=`), decoded.
fn decoded(value: &str) -> Option<String> {
    match value.strip_prefix("=?base64?").and_then(|v| v.strip_suffix("?=")) {
        Some(encoded) => String::from_utf8(base64::engine::general_purpose::STANDARD.decode(encoded).ok()?).ok(),
        None => Some(value.to_owned()),
    }
}

/// What the 2026-07-28 revision asks a server to check of a request's headers against its body.
fn check_modern(headers: &HeaderMap, req: &Value) -> Result<(), (i64, String, Option<Value>)> {
    let mismatch = |what: String| Err((HEADER_MISMATCH, what, None));
    let in_body = req["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"].as_str();
    let Some(version) = header(headers, "mcp-protocol-version") else {
        return mismatch("the MCP-Protocol-Version header is missing".into());
    };
    let Some(in_body) = in_body else {
        return Err((INVALID_PARAMS, "_meta has no io.modelcontextprotocol/protocolVersion".into(), None));
    };
    if version != in_body {
        return mismatch(format!(
            "Header mismatch: MCP-Protocol-Version header value '{version}' does not match body value '{in_body}'"
        ));
    }
    if version != MODERN {
        let data = json!({ "supported": [MODERN, LEGACY], "requested": version });
        return Err((UNSUPPORTED_VERSION, "Unsupported protocol version".into(), Some(data)));
    }
    let method = req["method"].as_str().unwrap_or_default();
    match header(headers, "mcp-method") {
        Some(m) if m == method => {}
        Some(m) => {
            return mismatch(format!(
                "Header mismatch: Mcp-Method header value '{m}' does not match body value '{method}'"
            ));
        }
        None => return mismatch("the Mcp-Method header is missing".into()),
    }
    let name = match method {
        "tools/call" | "prompts/get" => req["params"]["name"].as_str(),
        "resources/read" => req["params"]["uri"].as_str(),
        _ => return Ok(()),
    };
    match (header(headers, "mcp-name").and_then(decoded), name) {
        (Some(h), Some(b)) if h == b => Ok(()),
        (Some(h), b) => mismatch(format!(
            "Header mismatch: Mcp-Name header value '{h}' does not match body value '{}'",
            b.unwrap_or("")
        )),
        (None, _) => mismatch("the Mcp-Name header is missing".into()),
    }
}

/// What the answer to one request needs to know of it, for the record of a tool call.
struct Call<'a> {
    gw: &'a Gw,
    caller: &'a Caller,
    headers: &'a HeaderMap,
    era: Era,
    started: Instant,
    request_bytes: u64,
}

impl Call<'_> {
    /// The record of a `tools/call` of `tool` on `target`, with the caller, its attribution and
    /// the hash of the arguments. What came of the call is the caller's to fill in.
    fn record(&self, req: &Value, target: &str, tool: &str) -> ToolCallRecord {
        let mut r = ToolCallRecord::new(uuid::Uuid::now_v7(), &self.gw.boot.instance_id);
        r.organisation_id = self.caller.workspace.organisation_id.clone();
        r.workspace_id = self.caller.workspace.id.clone();
        r.key_id = self.caller.key.id.clone();
        GatewayParams::from_headers(self.headers).unwrap_or_default().attribute_tool(self.caller, &mut r);
        r.protocol = protocol::MCP.into();
        r.protocol_version = match self.era {
            Era::Modern => MODERN.to_owned(),
            Era::Legacy => header(self.headers, "mcp-protocol-version")
                .filter(|v| v.len() <= MAX_VERSION_CHARS)
                .unwrap_or_default()
                .to_owned(),
        };
        r.target = target.to_owned();
        r.operation = tool.to_owned();
        r.exposed_name = tool.to_owned();
        let arguments = &req["params"]["arguments"];
        if !arguments.is_null()
            && let Some(kek) = self.gw.boot.keks.first()
        {
            r.arguments_hmac = arguments_hmac(kek, &r.workspace_id, arguments);
        }
        r.request_bytes = self.request_bytes;
        r
    }

    /// Writes the record with the size of the answer and the time the whole request took.
    fn write(&self, mut r: ToolCallRecord, response_bytes: usize) {
        r.response_bytes = response_bytes as u64;
        r.total_ms = self.started.elapsed().as_secs_f32() * 1000.0;
        self.gw.record_tool_call(&r);
    }
}

/// The record of a tool call in progress, written however the request ends. When the caller
/// goes away before the answer, axum drops the handler and with it this guard, and the record
/// says `cancelled`. The call counts among the instance's calls in progress meanwhile.
struct Pending<'a> {
    call: &'a Call<'a>,
    record: Option<ToolCallRecord>,
}

impl<'a> Pending<'a> {
    fn new(call: &'a Call<'a>, record: ToolCallRecord) -> Self {
        call.gw.live.started();
        Self { call, record: Some(record) }
    }

    fn record(&mut self) -> &mut ToolCallRecord {
        self.record.as_mut().expect("a pending record is there until it is written")
    }

    /// Writes the record with the answer's size, and returns the answer.
    fn finish(mut self, (answer, bytes): (Response, usize)) -> Response {
        if let Some(r) = self.record.take() {
            self.call.write(r, bytes);
        }
        answer
    }
}

impl Drop for Pending<'_> {
    fn drop(&mut self) {
        self.call.gw.live.finished();
        if let Some(mut r) = self.record.take() {
            r.status = pacod_gw_events::usage::status::CANCELLED.into();
            r.error_type = "cancelled".into();
            self.call.write(r, 0);
        }
    }
}

pub async fn handle(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    serve(&gw, client, req, None).await
}

pub async fn handle_bundle(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(bundle): Path<String>,
    req: Request,
) -> Response {
    serve(&gw, client, req, Some(bundle)).await
}

async fn serve(gw: &Gw, client: std::net::IpAddr, req: Request, bundle: Option<String>) -> Response {
    let started = Instant::now();
    let (parts, body) = req.into_parts();
    let caller = match identify(gw, &parts.headers, Some(client)) {
        Ok(caller) => caller,
        Err(e) => return e.response(Envelope::Gateway, ""),
    };
    // The minimal gateway calls no tool server and has no broker: it has no MCP server.
    if gw.tools.is_none() && gw.boot.broker.is_none() {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "this gateway serves no tools" }))).into_response();
    }
    let limit = gw.boot.max_body_bytes;
    let bytes = match http_body_util::Limited::new(body, limit).collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => {
            return (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({ "error": format!("at most {limit} bytes") })))
                .into_response();
        }
    };
    let req = match message(&bytes) {
        Ok(req) => req,
        Err((code, why)) => {
            let era = era_of(&parts.headers, &Value::Null);
            return rpc_error(era, &Value::Null, code, why, None, StatusCode::BAD_REQUEST).0;
        }
    };
    let era = era_of(&parts.headers, &req);
    let call = Call { gw, caller: &caller, headers: &parts.headers, era, started, request_bytes: bytes.len() as u64 };
    if era == Era::Modern
        && let Err((code, why, data)) = check_modern(&parts.headers, &req)
    {
        crate::telemetry::metrics::record_mcp(server::label(&req), "refused");
        let answer = rpc_error(era, &req["id"], code, &why, data, StatusCode::BAD_REQUEST);
        // A tool call refused for its form is a call all the same, and is recorded.
        if req["method"] == "tools/call" {
            let mut pending =
                Pending::new(&call, call.record(&req, "", req["params"]["name"].as_str().unwrap_or_default()));
            server::refused(pending.record(), pacod_gw_events::tool::verdict::INVALID, code, &why);
            pending.record().http_status = 400;
            return pending.finish(answer);
        }
        return answer.0;
    }
    // A notification is taken and needs no answer.
    if req.get("id").is_none() {
        return StatusCode::ACCEPTED.into_response();
    }
    server::answer(&call, req, bundle.as_deref()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (name, value) in pairs {
            h.insert(*name, value.parse().unwrap());
        }
        h
    }

    #[test]
    fn one_message_is_taken_and_batches_are_not() {
        assert!(message(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).is_ok());
        assert_eq!(message(br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#).unwrap_err().0, INVALID_REQUEST);
        assert_eq!(message(b"7").unwrap_err().0, INVALID_REQUEST);
        assert_eq!(message(b"{").unwrap_err().0, PARSE_ERROR);
    }

    #[test]
    fn a_request_speaks_the_revision_its_meta_or_its_handshake_says() {
        let modern = json!({ "method": "tools/list", "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": MODERN } } });
        assert_eq!(era_of(&HeaderMap::new(), &modern), Era::Modern);
        assert_eq!(era_of(&HeaderMap::new(), &json!({ "method": "initialize" })), Era::Legacy);
        let legacy = json!({ "method": "tools/list", "params": {} });
        assert_eq!(era_of(&headers(&[("mcp-protocol-version", "2025-11-25")]), &legacy), Era::Legacy);
        assert_eq!(era_of(&headers(&[("mcp-protocol-version", "2026-07-28")]), &legacy), Era::Modern);
    }

    #[test]
    fn the_headers_of_a_current_request_must_say_what_its_body_says() {
        let req = json!({ "method": "tools/call", "params": { "name": "gh__search",
                          "_meta": { "io.modelcontextprotocol/protocolVersion": MODERN } } });
        let good = [("mcp-protocol-version", MODERN), ("mcp-method", "tools/call"), ("mcp-name", "gh__search")];
        assert!(check_modern(&headers(&good), &req).is_ok());
        let encoded = [
            ("mcp-protocol-version", MODERN),
            ("mcp-method", "tools/call"),
            ("mcp-name", "=?base64?Z2hfX3NlYXJjaA==?="),
        ];
        assert!(check_modern(&headers(&encoded), &req).is_ok(), "a name may come in base64");
        let other = [("mcp-protocol-version", MODERN), ("mcp-method", "tools/call"), ("mcp-name", "gh__delete")];
        assert_eq!(check_modern(&headers(&other), &req).unwrap_err().0, HEADER_MISMATCH);
        let missing = [("mcp-protocol-version", MODERN), ("mcp-name", "gh__search")];
        assert_eq!(check_modern(&headers(&missing), &req).unwrap_err().0, HEADER_MISMATCH);
        let future = json!({ "method": "tools/list", "params": { "_meta": { "io.modelcontextprotocol/protocolVersion": "2027-01-01" } } });
        let (code, _, data) =
            check_modern(&headers(&[("mcp-protocol-version", "2027-01-01"), ("mcp-method", "tools/list")]), &future)
                .unwrap_err();
        assert_eq!(code, UNSUPPORTED_VERSION);
        assert_eq!(data.unwrap()["requested"], "2027-01-01");
    }
}
