//! A mock MCP tool server, for the gateway's tests of tool servers (runbook section 21) and for
//! `gw-mock`, which serves one of each revision in the development stack.
//!
//! It speaks one revision of the protocol at a time, as real servers do:
//!
//! - `2026-07-28` (stateless): `server/discover`, `tools/list` and `tools/call`; every request
//!   must carry the revision in `MCP-Protocol-Version`, and a call's `Mcp-Name` must be its tool.
//! - `2025-11-25`: `initialize` first, which gives a session id; every other request must carry
//!   it, and a forgotten session answers `404`.
//!
//! Tool lists come two to a page. Its tools: `echo` (answers `text`), `big` (answers `bytes`
//! bytes of text), `fail` (answers with `isError`), `picture` (answers an image) and
//! `repo.search` (a name providers do not accept); `gw-mock` adds `wait` (answers `text` after
//! `ms` milliseconds, for load tests). A test can change the tools, ask for a credential
//! header, answer with server-sent events, have the server ask the caller for input, and forget
//! the sessions.
//!
//! [`MockTools`] is the server's state and its routes, to mount on any router (`gw-mock` mounts
//! two on the mock provider's port); [`MockToolServer`] serves one on a port of its own.

use std::collections::{HashSet, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::watch;
use tokio_stream::StreamExt as _;

pub const MODERN: &str = "2026-07-28";
pub const LEGACY: &str = "2025-11-25";

/// The longest a `wait` call waits, in milliseconds.
pub const MAX_WAIT_MS: u64 = 60_000;
/// Sessions of the 2025-11-25 revision kept at most; the oldest is forgotten first, as a server
/// whose sessions time out would. A load test opens one per call when the gateway does.
pub const MAX_SESSIONS: usize = 10_000;

/// Which revision a mock speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Era {
    Modern,
    Legacy,
}

/// One request the mock received.
#[derive(Debug, Clone)]
pub struct SeenRequest {
    pub headers: HeaderMap,
    pub body: Value,
}

/// Session ids, with the order they were given in.
#[derive(Default)]
struct Sessions {
    ids: HashSet<String>,
    order: VecDeque<String>,
}

impl Sessions {
    fn open(&mut self, id: String) {
        while self.order.len() >= MAX_SESSIONS {
            if let Some(old) = self.order.pop_front() {
                self.ids.remove(&old);
            }
        }
        self.ids.insert(id.clone());
        self.order.push_back(id);
    }
}

struct Shared {
    era: Era,
    tools: Mutex<Vec<Value>>,
    /// The tools the mock started with, which a reset restores.
    initial: Vec<Value>,
    /// How many times the control route changed a definition, so that each change differs.
    changes: AtomicU64,
    credential: Mutex<Option<(String, String)>>,
    sse: AtomicBool,
    keep_open: AtomicBool,
    crlf: AtomicBool,
    ask_input: AtomicBool,
    sessions: Mutex<Sessions>,
    next_session: AtomicU64,
    requests: Mutex<VecDeque<SeenRequest>>,
    /// How many received requests are kept; 0 keeps none, without taking the lock.
    capture_limit: AtomicUsize,
}

/// A mock tool server's state, its routes and its controls. Clones share the state.
#[derive(Clone)]
pub struct MockTools {
    shared: Arc<Shared>,
}

/// A [`MockTools`] on a port of its own, at `/mcp`. It stops when dropped.
pub struct MockToolServer {
    addr: SocketAddr,
    tools: MockTools,
    _shutdown: watch::Sender<bool>,
}

/// The mock's tools when a test sets none. Tests' hashes depend on them: `gw-mock` adds its
/// own in [`mock_tools`] instead of here.
pub fn default_tools() -> Vec<Value> {
    let text = json!({ "type": "object", "properties": { "text": { "type": "string" } }, "required": ["text"] });
    vec![
        json!({ "name": "echo", "description": "Answers the text it is given.", "inputSchema": text }),
        json!({ "name": "big", "description": "Answers a text of the given size.",
                "inputSchema": { "type": "object", "properties": { "bytes": { "type": "integer" } } } }),
        json!({ "name": "fail", "description": "Answers with an error result.", "inputSchema": { "type": "object" } }),
        json!({ "name": "picture", "description": "Answers an image.", "inputSchema": { "type": "object" } }),
        json!({ "name": "repo.search", "description": "Searches repositories.", "inputSchema": text,
                "annotations": { "readOnlyHint": true }, "_meta": { "vendor": "x" } }),
    ]
}

/// The tools of `gw-mock`: [`default_tools`] and `wait`, which answers `text` after `ms`
/// milliseconds (at most [`MAX_WAIT_MS`]), the latency a load test asks for.
pub fn mock_tools() -> Vec<Value> {
    let mut tools = default_tools();
    tools.push(json!({
        "name": "wait",
        "description": "Waits ms milliseconds, at most 60000, then answers the text it is given.",
        "inputSchema": { "type": "object",
                         "properties": { "ms": { "type": "integer", "minimum": 0, "maximum": MAX_WAIT_MS },
                                         "text": { "type": "string" } },
                         "required": ["ms"] }
    }));
    tools
}

impl MockTools {
    /// A server of `era` with [`default_tools`].
    pub fn new(era: Era) -> Self {
        Self::with_tools(era, default_tools())
    }

    /// A server of `era` with `tools`, which [`Self::reset_tools`] goes back to.
    pub fn with_tools(era: Era, tools: Vec<Value>) -> Self {
        let shared = Arc::new(Shared {
            era,
            tools: Mutex::new(tools.clone()),
            initial: tools,
            changes: AtomicU64::new(0),
            credential: Mutex::default(),
            sse: AtomicBool::new(false),
            keep_open: AtomicBool::new(false),
            crlf: AtomicBool::new(false),
            ask_input: AtomicBool::new(false),
            sessions: Mutex::default(),
            next_session: AtomicU64::new(1),
            requests: Mutex::default(),
            capture_limit: AtomicUsize::new(usize::MAX),
        });
        Self { shared }
    }

    pub fn era(&self) -> Era {
        self.shared.era
    }

    /// The server's endpoint at `path` (`POST`; other methods answer `405`).
    pub fn routes(&self, path: &str) -> Router {
        Router::new().route(path, post(handle)).with_state(self.shared.clone())
    }

    /// The control endpoint at `path`: `POST` with a JSON array replaces the tools; `POST`
    /// without a body changes `echo`'s description (the first tool's without `echo`), each time
    /// to a new one, so that its hash changes; `DELETE` restores the tools the mock started
    /// with. Both answer the tools now listed.
    pub fn control_routes(&self, path: &str) -> Router {
        Router::new().route(path, post(change_tools).delete(reset_tools)).with_state(self.shared.clone())
    }

    pub fn set_tools(&self, tools: Vec<Value>) {
        *self.shared.tools.lock() = tools;
    }

    /// The tools the server lists now.
    pub fn tools(&self) -> Vec<Value> {
        self.shared.tools.lock().clone()
    }

    /// Goes back to the tools the mock started with.
    pub fn reset_tools(&self) {
        *self.shared.tools.lock() = self.shared.initial.clone();
    }

    /// Answers `401` to requests without this header and value.
    pub fn require(&self, header: &str, value: &str) {
        *self.shared.credential.lock() = Some((header.to_ascii_lowercase(), value.to_owned()));
    }

    /// Answers with a stream of server-sent events: a progress notification, then the answer.
    pub fn answer_with_events(&self, on: bool) {
        self.shared.sse.store(on, Ordering::SeqCst);
    }

    /// With [`Self::answer_with_events`]: leaves each stream open after the answer, as a server
    /// may, so that only a client that stops at the answer gets on.
    pub fn keep_streams_open(&self, on: bool) {
        self.shared.keep_open.store(on, Ordering::SeqCst);
    }

    /// With [`Self::answer_with_events`]: ends the lines of its events with CRLF, as the
    /// official SDKs write them, and sends a stream it keeps open in chunks of seven bytes, so
    /// that line endings and characters fall across chunks.
    pub fn frame_with_crlf(&self, on: bool) {
        self.shared.crlf.store(on, Ordering::SeqCst);
    }

    /// Answers a tool call with a request for input from the caller (sampling).
    pub fn ask_for_input(&self, on: bool) {
        self.shared.ask_input.store(on, Ordering::SeqCst);
    }

    /// Forgets every session, as a restarted server would.
    pub fn forget_sessions(&self) {
        *self.shared.sessions.lock() = Sessions::default();
    }

    /// Keeps only the `limit` most recent requests from now on; 0 keeps none. The default keeps
    /// every request, which a mock under load should not do.
    pub fn set_capture_limit(&self, limit: usize) {
        self.shared.capture_limit.store(limit, Ordering::Relaxed);
        let mut kept = self.shared.requests.lock();
        while kept.len() > limit {
            kept.pop_front();
        }
    }

    pub fn requests(&self) -> Vec<SeenRequest> {
        self.shared.requests.lock().iter().cloned().collect()
    }

    /// The tool calls received, by tool name.
    pub fn calls(&self) -> Vec<SeenRequest> {
        self.requests().into_iter().filter(|r| r.body["method"] == "tools/call").collect()
    }
}

impl MockToolServer {
    pub async fn spawn(era: Era) -> Self {
        let tools = MockTools::new(era);
        let app = tools.routes("/mcp");
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("the mock tool server binds");
        let addr = listener.local_addr().expect("an address");
        let (tx, mut rx) = watch::channel(false);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.changed().await;
                })
                .await;
        });
        Self { addr, tools, _shutdown: tx }
    }

    /// The server's MCP endpoint.
    pub fn url(&self) -> String {
        format!("http://{}/mcp", self.addr)
    }
}

impl std::ops::Deref for MockToolServer {
    type Target = MockTools;

    fn deref(&self) -> &MockTools {
        &self.tools
    }
}

impl Shared {
    fn capture(&self, request: impl FnOnce() -> SeenRequest) {
        let limit = self.capture_limit.load(Ordering::Relaxed);
        if limit == 0 {
            return;
        }
        let request = request();
        let mut kept = self.requests.lock();
        while kept.len() >= limit {
            kept.pop_front();
        }
        kept.push_back(request);
    }
}

async fn change_tools(State(shared): State<Arc<Shared>>, body: Bytes) -> Response {
    let mut tools = shared.tools.lock();
    if body.iter().any(|b| !b.is_ascii_whitespace()) {
        match serde_json::from_slice::<Vec<Value>>(&body) {
            Ok(given) => *tools = given,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    axum::Json(json!({ "error": format!("a JSON array of tools: {e}") })),
                )
                    .into_response();
            }
        }
    } else {
        let n = shared.changes.fetch_add(1, Ordering::SeqCst) + 1;
        let at = tools.iter().position(|t| t["name"] == "echo").unwrap_or(0);
        let Some(tool) = tools.get_mut(at) else {
            return (StatusCode::CONFLICT, axum::Json(json!({ "error": "the server lists no tool to change" })))
                .into_response();
        };
        let was = shared.initial.iter().find(|t| t["name"] == tool["name"]).unwrap_or(tool);
        let description = format!("{} Revision {}.", was["description"].as_str().unwrap_or_default(), n + 1);
        tool["description"] = json!(description);
    }
    axum::Json(json!({ "tools": *tools })).into_response()
}

async fn reset_tools(State(shared): State<Arc<Shared>>) -> Response {
    let mut tools = shared.tools.lock();
    *tools = shared.initial.clone();
    axum::Json(json!({ "tools": *tools })).into_response()
}

fn rpc(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

fn rpc_error(id: &Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({ "code": code, "message": message });
    if let Some(d) = data {
        error["data"] = d;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": error })
}

fn answer(shared: &Shared, message: Value, extra: Option<(&'static str, String)>) -> Response {
    let mut resp = if shared.sse.load(Ordering::SeqCst) {
        let progress = json!({ "jsonrpc": "2.0", "method": "notifications/progress",
                               "params": { "progressToken": 1, "progress": 0.5 } });
        let mut body = format!("event: message\ndata: {progress}\n\nevent: message\ndata: {message}\n\n");
        let crlf = shared.crlf.load(Ordering::SeqCst);
        if crlf {
            body = body.replace('\n', "\r\n");
        }
        if shared.keep_open.load(Ordering::SeqCst) {
            let size = if crlf { 7 } else { body.len() };
            let chunks: Vec<_> = body
                .as_bytes()
                .chunks(size)
                .map(|c| Ok::<_, std::io::Error>(axum::body::Bytes::copy_from_slice(c)))
                .collect();
            let events = tokio_stream::iter(chunks).chain(tokio_stream::pending());
            return ([(header::CONTENT_TYPE, "text/event-stream")], axum::body::Body::from_stream(events))
                .into_response();
        }
        ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response()
    } else {
        axum::Json(message).into_response()
    };
    if let Some((name, value)) = extra
        && let Ok(v) = value.parse()
    {
        resp.headers_mut().insert(name, v);
    }
    resp
}

async fn handle(State(shared): State<Arc<Shared>>, headers: HeaderMap, body: Bytes) -> Response {
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "not JSON").into_response();
    };
    shared.capture(|| SeenRequest { headers: headers.clone(), body: req.clone() });
    let required = shared.credential.lock().clone();
    if let Some((name, value)) = required
        && headers.get(&name).and_then(|v| v.to_str().ok()) != Some(value.as_str())
    {
        return (StatusCode::UNAUTHORIZED, "a credential is needed").into_response();
    }
    let id = req.get("id").cloned().unwrap_or(Value::Null);
    let method = req["method"].as_str().unwrap_or_default();
    let header = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();

    match shared.era {
        Era::Modern => {
            if method == "initialize" {
                let body =
                    rpc_error(&id, -32022, "unsupported protocol version", Some(json!({ "supported": [MODERN] })));
                return (StatusCode::BAD_REQUEST, axum::Json(body)).into_response();
            }
            if header("mcp-protocol-version") != MODERN
                || req["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"] != MODERN
            {
                let body =
                    rpc_error(&id, -32022, "unsupported protocol version", Some(json!({ "supported": [MODERN] })));
                return (StatusCode::BAD_REQUEST, axum::Json(body)).into_response();
            }
            if header("mcp-method") != method
                || (method == "tools/call" && header("mcp-name") != req["params"]["name"].as_str().unwrap_or_default())
            {
                let body = rpc_error(&id, -32020, "the headers do not match the body", None);
                return (StatusCode::BAD_REQUEST, axum::Json(body)).into_response();
            }
        }
        Era::Legacy => {
            if method == "initialize" {
                let sid = format!("sess-{}", shared.next_session.fetch_add(1, Ordering::SeqCst));
                shared.sessions.lock().open(sid.clone());
                let result = json!({ "protocolVersion": LEGACY, "capabilities": { "tools": {} },
                                     "serverInfo": { "name": "mock", "version": "1" } });
                return answer(&shared, rpc(&id, result), Some(("mcp-session-id", sid)));
            }
            let sid = header("mcp-session-id");
            if sid.is_empty() {
                return (StatusCode::BAD_REQUEST, "Bad Request: no valid session id").into_response();
            }
            if !shared.sessions.lock().ids.contains(&sid) {
                return (StatusCode::NOT_FOUND, "session not found").into_response();
            }
            if method == "notifications/initialized" {
                return StatusCode::ACCEPTED.into_response();
            }
        }
    }

    match method {
        "server/discover" => answer(
            &shared,
            rpc(
                &id,
                json!({ "supportedVersions": [MODERN], "capabilities": { "tools": {} },
                             "serverInfo": { "name": "mock", "version": "1" } }),
            ),
            None,
        ),
        "tools/list" => {
            let tools = shared.tools.lock().clone();
            let start: usize = req["params"]["cursor"].as_str().and_then(|c| c.parse().ok()).unwrap_or(0);
            let page: Vec<Value> = tools.iter().skip(start).take(2).cloned().collect();
            let mut result = json!({ "tools": page });
            if start + 2 < tools.len() {
                result["nextCursor"] = json!((start + 2).to_string());
            }
            answer(&shared, rpc(&id, result), None)
        }
        "tools/call" => {
            if shared.ask_input.load(Ordering::SeqCst) {
                let ask = json!({ "jsonrpc": "2.0", "id": "srv-1", "method": "sampling/createMessage", "params": {} });
                let body = format!("event: message\ndata: {ask}\n\n");
                return ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response();
            }
            let name = req["params"]["name"].as_str().unwrap_or_default();
            let args = &req["params"]["arguments"];
            let result = match name {
                "echo" | "repo.search" => {
                    json!({ "content": [{ "type": "text", "text": args["text"].as_str().unwrap_or_default() }],
                            "_meta": { "server": "mock" } })
                }
                "big" => {
                    let n = args["bytes"].as_u64().unwrap_or(1) as usize;
                    json!({ "content": [{ "type": "text", "text": "x".repeat(n) }] })
                }
                "fail" => json!({ "content": [{ "type": "text", "text": "it went wrong" }], "isError": true }),
                "picture" => {
                    json!({ "content": [{ "type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png" }] })
                }
                "wait" => {
                    let Some(ms) = args["ms"].as_u64().filter(|ms| *ms <= MAX_WAIT_MS) else {
                        let why = format!("wait: ms must be an integer from 0 to {MAX_WAIT_MS}");
                        return answer(&shared, rpc_error(&id, -32602, &why, None), None);
                    };
                    tokio::time::sleep(Duration::from_millis(ms)).await;
                    json!({ "content": [{ "type": "text", "text": args["text"].as_str().unwrap_or_default() }] })
                }
                _ => return answer(&shared, rpc_error(&id, -32602, &format!("unknown tool: {name}"), None), None),
            };
            answer(&shared, rpc(&id, result), None)
        }
        _ => answer(&shared, rpc_error(&id, -32601, "method not found", None), None),
    }
}
