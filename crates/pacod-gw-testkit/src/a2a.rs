//! A mock A2A agent (A2A 1.0, JSON-RPC binding), for the gateway's tests of remote agents
//! (runbook section 21.4).
//!
//! It serves its card at `/.well-known/agent-card.json` with one JSON-RPC interface (and a
//! tenant), answers `SendMessage` with a completed task that echoes the message, streams
//! `SendStreamingMessage` as two events, keeps its tasks for `GetTask` and `CancelTask`, and
//! lists every task it has for `ListTasks`, to anyone: which is why the gateway must not pass
//! that on. Task ids are its own (`task-1`, …). A test can change the card and ask for a
//! credential, and reads what reached the agent.

use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use parking_lot::Mutex;
use serde_json::{Value, json};
use tokio::sync::watch;

/// One request the mock received.
#[derive(Debug, Clone)]
pub struct SeenRequest {
    pub headers: HeaderMap,
    pub body: Value,
}

struct Shared {
    card: Mutex<Value>,
    credential: Mutex<Option<(String, String)>>,
    tasks: Mutex<Vec<Value>>,
    next: AtomicU64,
    crlf: AtomicBool,
    requests: Mutex<Vec<SeenRequest>>,
}

pub struct MockAgent {
    addr: SocketAddr,
    shared: Arc<Shared>,
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

impl MockAgent {
    pub async fn spawn() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("the mock agent binds");
        let addr = listener.local_addr().expect("an address");
        let shared = Arc::new(Shared {
            card: Mutex::new(default_card(&format!("http://{addr}"))),
            credential: Mutex::default(),
            tasks: Mutex::default(),
            next: AtomicU64::new(1),
            crlf: AtomicBool::new(false),
            requests: Mutex::default(),
        });
        let app = Router::new()
            .route("/.well-known/agent-card.json", get(card))
            .route("/a2a", post(handle))
            .with_state(shared.clone());
        let (tx, mut rx) = watch::channel(false);
        tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = rx.changed().await;
                })
                .await;
        });
        Self { addr, shared, _shutdown: tx }
    }

    pub fn card_url(&self) -> String {
        format!("http://{}/.well-known/agent-card.json", self.addr)
    }

    pub fn set_card(&self, card: Value) {
        *self.shared.card.lock() = card;
    }

    pub fn card(&self) -> Value {
        self.shared.card.lock().clone()
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

    pub fn requests(&self) -> Vec<SeenRequest> {
        self.shared.requests.lock().clone()
    }
}

async fn card(State(shared): State<Arc<Shared>>) -> Response {
    axum::Json(shared.card.lock().clone()).into_response()
}

fn reply(id: &Value, result: Value) -> Response {
    axum::Json(json!({ "jsonrpc": "2.0", "id": id, "result": result })).into_response()
}

fn error(id: &Value, code: i64, message: &str) -> Response {
    axum::Json(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })).into_response()
}

async fn handle(State(shared): State<Arc<Shared>>, headers: HeaderMap, body: axum::body::Bytes) -> Response {
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "not JSON").into_response();
    };
    shared.requests.lock().push(SeenRequest { headers: headers.clone(), body: req.clone() });
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
            shared.tasks.lock().push(task.clone());
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
            match tasks.iter_mut().find(|t| t["id"] == wanted) {
                Some(task) => {
                    if req["method"] == "CancelTask" {
                        task["status"] = json!({ "state": "TASK_STATE_CANCELED" });
                    }
                    reply(&id, task.clone())
                }
                None => error(&id, -32001, "task not found"),
            }
        }
        "ListTasks" => reply(&id, json!({ "tasks": shared.tasks.lock().clone() })),
        _ => error(&id, -32601, "method not found"),
    }
}
