//! A mock A2A agent (A2A 1.0, JSON-RPC binding), for the gateway's tests of remote agents
//! (runbook section 21.4) and for `gw-mock`, which serves one in the development stack.
//!
//! It serves its card at `/.well-known/agent-card.json` with one JSON-RPC interface (and a
//! tenant), answers `SendMessage` with a completed task that echoes the message, streams
//! `SendStreamingMessage` as two events, keeps its tasks for `GetTask` and `CancelTask`, and
//! lists every task it has for `ListTasks`, to anyone: which is why the gateway must not pass
//! that on. Task ids are its own (`task-1`, …); it keeps the newest [`MAX_TASKS`]. A message
//! whose `metadata` holds `mock/delay_ms` is answered that many milliseconds later (at most
//! [`MAX_DELAY_MS`]), the latency a load test asks for. A test can change the card and ask for
//! a credential, and reads what reached the agent.
//!
//! [`MockAgents`] is the agent's state and its routes, to mount on any router; [`MockAgent`]
//! serves one on a port of its own. Mounted without a fixed address (`gw-mock`), the card names
//! the interface at the host the request for it named.

use std::collections::{HashMap, VecDeque};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::watch;

/// Tasks kept at most; the oldest is forgotten first.
pub const MAX_TASKS: usize = 10_000;
/// The longest `mock/delay_ms` waits.
pub const MAX_DELAY_MS: u64 = 60_000;

/// One request the mock received.
#[derive(Debug, Clone)]
pub struct SeenRequest {
    pub headers: HeaderMap,
    pub body: Value,
}

/// The card the agent serves.
#[derive(Debug, Clone)]
enum Card {
    /// [`default_card`], its description changed `changes` times by the control route.
    Default { changes: u64 },
    /// A card a test or the control route gave. An interface URL that is a path is taken as
    /// below the agent's address.
    Given(Value),
}

/// The tasks, by id, with the order they came in.
#[derive(Default)]
struct Tasks {
    by_id: HashMap<String, Value>,
    order: VecDeque<String>,
}

impl Tasks {
    /// Keeps a task. A message to a task the agent has (by its `taskId`) leaves the first one
    /// as it was.
    fn keep(&mut self, task: Value) {
        let id = task["id"].as_str().unwrap_or_default().to_owned();
        if self.by_id.contains_key(&id) {
            return;
        }
        while self.order.len() >= MAX_TASKS {
            if let Some(old) = self.order.pop_front() {
                self.by_id.remove(&old);
            }
        }
        self.by_id.insert(id.clone(), task);
        self.order.push_back(id);
    }
}

struct Shared {
    /// The agent's address; `None` takes it from the request's `Host`.
    base: Option<String>,
    card: Mutex<Card>,
    credential: Mutex<Option<(String, String)>>,
    tasks: Mutex<Tasks>,
    next: AtomicU64,
    crlf: AtomicBool,
    requests: Mutex<VecDeque<SeenRequest>>,
    /// How many received requests are kept; 0 keeps none, without taking the lock.
    capture_limit: AtomicUsize,
}

/// A mock agent's state, its routes and its controls. Clones share the state.
#[derive(Clone)]
pub struct MockAgents {
    shared: Arc<Shared>,
}

/// A [`MockAgents`] on a port of its own. It stops when dropped.
pub struct MockAgent {
    addr: SocketAddr,
    agents: MockAgents,
    _shutdown: watch::Sender<bool>,
}

/// The mock's card, with its interface at `base`.
pub fn default_card(base: &str) -> Value {
    json!({
        "name": "Researcher",
        "description": "Finds things and says what it found.",
        "version": "1.0.0",
        "supportedInterfaces": [
            { "url": format!("{base}/a2a"), "protocolBinding": "JSONRPC", "protocolVersion": "1.0", "tenant": "acme" }
        ],
        "capabilities": { "streaming": true, "pushNotifications": true, "extendedAgentCard": false },
        "securitySchemes": { "key": { "apiKeySecurityScheme": { "name": "X-Agent-Key", "location": "header" } } },
        "securityRequirements": [{ "schemes": { "key": { "list": [] } } }],
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"],
        "skills": [{ "id": "search", "name": "Search", "description": "Searches the web.", "tags": ["search"] }],
        "signatures": [{ "protected": "eyJhbGciOiJFUzI1NiJ9", "signature": "c2ln" }]
    })
}

impl Shared {
    fn new(base: Option<String>) -> Self {
        Self {
            base,
            card: Mutex::new(Card::Default { changes: 0 }),
            credential: Mutex::default(),
            tasks: Mutex::default(),
            next: AtomicU64::new(1),
            crlf: AtomicBool::new(false),
            requests: Mutex::default(),
            capture_limit: AtomicUsize::new(usize::MAX),
        }
    }

    /// The card as served to a request for `host`.
    fn card_for(&self, host: Option<&str>) -> Value {
        let base = self.base.clone().unwrap_or_else(|| format!("http://{}", host.unwrap_or("localhost")));
        match self.card.lock().clone() {
            Card::Default { changes } => {
                let mut card = default_card(&base);
                if changes > 0 {
                    card["description"] =
                        json!(format!("Finds things and says what it found. Revision {}.", changes + 1));
                }
                card
            }
            Card::Given(mut card) => {
                for iface in card["supportedInterfaces"].as_array_mut().into_iter().flatten() {
                    if let Some(path) = iface["url"].as_str().filter(|u| u.starts_with('/')) {
                        iface["url"] = json!(format!("{base}{path}"));
                    }
                }
                card
            }
        }
    }

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

impl MockAgents {
    /// An agent whose card names the interface at the host each request for the card names.
    pub fn new() -> Self {
        Self { shared: Arc::new(Shared::new(None)) }
    }

    /// An agent at `base` (`http://host:port`), whose card names the interface there.
    pub fn at(base: &str) -> Self {
        Self { shared: Arc::new(Shared::new(Some(base.trim_end_matches('/').to_owned()))) }
    }

    /// The card at `/.well-known/agent-card.json` and the interface at `/a2a`.
    pub fn routes(&self) -> Router {
        Router::new()
            .route("/.well-known/agent-card.json", get(card))
            .route("/a2a", post(handle))
            .with_state(self.shared.clone())
    }

    /// The control endpoint at `path`: `POST` with a JSON object replaces the card; `POST`
    /// without a body changes the card's description, each time to a new one, so that its hash
    /// changes; `DELETE` goes back to the card the mock started with. Both answer the card.
    pub fn control_routes(&self, path: &str) -> Router {
        Router::new().route(path, post(change_card).delete(reset_card)).with_state(self.shared.clone())
    }

    pub fn set_card(&self, card: Value) {
        *self.shared.card.lock() = Card::Given(card);
    }

    /// The card as served at the agent's own address, or at `localhost` without one.
    pub fn card(&self) -> Value {
        self.shared.card_for(None)
    }

    /// Answers `401` to requests without this header and value.
    pub fn require(&self, header: &str, value: &str) {
        *self.shared.credential.lock() = Some((header.to_ascii_lowercase(), value.to_owned()));
    }

    /// Ends the lines of its streams' events with CRLF, as the official SDKs write them, and
    /// sends each stream in chunks of seven bytes, so that line endings and characters fall
    /// across chunks.
    pub fn frame_with_crlf(&self, on: bool) {
        self.shared.crlf.store(on, Ordering::SeqCst);
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
}

impl Default for MockAgents {
    fn default() -> Self {
        Self::new()
    }
}

impl MockAgent {
    pub async fn spawn() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("the mock agent binds");
        let addr = listener.local_addr().expect("an address");
        let agents = MockAgents::at(&format!("http://{addr}"));
        let app = agents.routes();
        let (tx, mut rx) = watch::channel(false);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.changed().await;
                })
                .await;
        });
        Self { addr, agents, _shutdown: tx }
    }

    pub fn card_url(&self) -> String {
        format!("http://{}/.well-known/agent-card.json", self.addr)
    }
}

impl std::ops::Deref for MockAgent {
    type Target = MockAgents;

    fn deref(&self) -> &MockAgents {
        &self.agents
    }
}

async fn change_card(State(shared): State<Arc<Shared>>, headers: HeaderMap, body: Bytes) -> Response {
    if body.iter().any(|b| !b.is_ascii_whitespace()) {
        match serde_json::from_slice::<Value>(&body) {
            Ok(card) if card.is_object() => *shared.card.lock() = Card::Given(card),
            _ => {
                return (StatusCode::BAD_REQUEST, axum::Json(json!({ "error": "the card must be a JSON object" })))
                    .into_response();
            }
        }
    } else {
        let mut card = shared.card.lock();
        *card = match &*card {
            Card::Default { changes } => Card::Default { changes: changes + 1 },
            Card::Given(given) => {
                let mut given = given.clone();
                let text = given["description"].as_str().unwrap_or_default().to_owned();
                given["description"] = json!(format!("{text} Changed."));
                Card::Given(given)
            }
        };
    }
    axum::Json(shared.card_for(host(&headers))).into_response()
}

async fn reset_card(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    *shared.card.lock() = Card::Default { changes: 0 };
    axum::Json(shared.card_for(host(&headers))).into_response()
}

fn host(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::HOST).and_then(|v| v.to_str().ok())
}

async fn card(State(shared): State<Arc<Shared>>, headers: HeaderMap) -> Response {
    axum::Json(shared.card_for(host(&headers))).into_response()
}

fn reply(id: &Value, result: Value) -> Response {
    axum::Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

fn error(id: &Value, code: i64, message: &str) -> Response {
    axum::Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })).into_response()
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
    let id = req["id"].clone();
    let params = &req["params"];
    match req["method"].as_str().unwrap_or_default() {
        method @ ("SendMessage" | "SendStreamingMessage") => {
            if let Some(ms) = params["metadata"]["mock/delay_ms"].as_u64() {
                tokio::time::sleep(Duration::from_millis(ms.min(MAX_DELAY_MS))).await;
            }
            let message = &params["message"];
            let n = shared.next.fetch_add(1, Ordering::SeqCst);
            let task_id = message["taskId"].as_str().map_or_else(|| format!("task-{n}"), str::to_owned);
            let context = message["contextId"].as_str().map_or_else(|| format!("ctx-{n}"), str::to_owned);
            let text = message["parts"][0]["text"].as_str().unwrap_or_default().to_owned();
            let mut echoed = message.clone();
            echoed["taskId"] = json!(task_id);
            echoed["contextId"] = json!(context);
            let task = json!({
                "id": task_id, "contextId": context,
                "status": { "state": "TASK_STATE_COMPLETED" },
                "history": [echoed],
                "artifacts": [{ "artifactId": format!("art-{n}"), "parts": [{ "text": format!("found: {text}") }] }]
            });
            shared.tasks.lock().keep(task.clone());
            if method == "SendMessage" {
                return reply(&id, json!({ "task": task }));
            }
            let mut working = task.clone();
            working["status"] = json!({ "state": "TASK_STATE_WORKING" });
            let first = json!({ "jsonrpc": "2.0", "id": id, "result": { "task": working } });
            let second = json!({ "jsonrpc": "2.0", "id": id, "result": { "statusUpdate": {
                "taskId": task_id, "contextId": context, "status": { "state": "TASK_STATE_COMPLETED" } } } });
            let body = format!("data: {first}\n\ndata: {second}\n\n");
            if !shared.crlf.load(Ordering::SeqCst) {
                return ([(header::CONTENT_TYPE, "text/event-stream")], body).into_response();
            }
            let chunks: Vec<_> = body
                .replace('\n', "\r\n")
                .as_bytes()
                .chunks(7)
                .map(|c| Ok::<_, std::io::Error>(axum::body::Bytes::copy_from_slice(c)))
                .collect();
            ([(header::CONTENT_TYPE, "text/event-stream")], axum::body::Body::from_stream(tokio_stream::iter(chunks)))
                .into_response()
        }
        "GetTask" | "CancelTask" => {
            let wanted = params["id"].as_str().unwrap_or_default();
            let mut tasks = shared.tasks.lock();
            match tasks.by_id.get_mut(wanted) {
                Some(task) => {
                    if req["method"] == "CancelTask" {
                        task["status"] = json!({ "state": "TASK_STATE_CANCELED" });
                    }
                    reply(&id, task.clone())
                }
                None => error(&id, -32001, "task not found"),
            }
        }
        "ListTasks" => {
            let tasks = shared.tasks.lock();
            let all: Vec<&Value> = tasks.order.iter().filter_map(|id| tasks.by_id.get(id)).collect();
            reply(&id, json!({ "tasks": all }))
        }
        _ => error(&id, -32601, "method not found"),
    }
}
