//! OpenAI's Realtime API at `GET /v1/realtime?model=…`, scripted:
//!
//! - `session.created` when the session opens, `session.updated` for each `session.update`;
//! - `response.create` is answered with `response.created`, a text delta and `response.done`
//!   with usage (audio and cached tokens included). A response whose `metadata.mock` is
//!   `"hold"` stays in progress until `response.cancel`, then ends `cancelled`;
//! - `input_audio_buffer.commit` starts a response of its own, as the provider's voice
//!   activity detection does.
//!
//! The handshake is captured like any request.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::server::Shared;

/// The usage of every completed response: 120 tokens in (80 of them audio; 40 cached, 20 of
/// those audio), 60 out (40 of them audio).
pub(crate) fn usage() -> Value {
    json!({
        "total_tokens": 180,
        "input_tokens": 120,
        "output_tokens": 60,
        "input_token_details": {
            "text_tokens": 40, "audio_tokens": 80, "image_tokens": 0, "cached_tokens": 40,
            "cached_tokens_details": { "text_tokens": 20, "audio_tokens": 20, "image_tokens": 0 }
        },
        "output_token_details": { "text_tokens": 20, "audio_tokens": 40 }
    })
}

pub(crate) async fn connect(State(shared): State<Arc<Shared>>, req: Request) -> Response {
    super::websocket::capture(&shared, &req);
    let model = req
        .uri()
        .query()
        .and_then(|q| q.split('&').find_map(|kv| kv.strip_prefix("model=")))
        .unwrap_or_default()
        .to_owned();
    let realtime = req
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.split(',').any(|p| p.trim() == "realtime"));
    let (mut parts, _) = req.into_parts();
    let upgrade =
        match <WebSocketUpgrade as axum::extract::FromRequestParts<()>>::from_request_parts(&mut parts, &()).await {
            Ok(u) => u,
            Err(e) => return e.into_response(),
        };
    let upgrade = if realtime { upgrade.protocols(["realtime"]) } else { upgrade };
    upgrade.on_upgrade(move |socket| session(socket, model))
}

struct Script {
    socket: WebSocket,
    events: u64,
    responses: u64,
    /// The response held until it is cancelled.
    held: Option<String>,
}

impl Script {
    async fn send(&mut self, mut event: Value) -> bool {
        self.events += 1;
        event["event_id"] = json!(format!("event_mock_{}", self.events));
        self.socket.send(Message::text(event.to_string())).await.is_ok()
    }

    /// Starts a response; ends it at once unless it is to be held.
    async fn respond(&mut self, hold: bool) -> bool {
        self.responses += 1;
        let id = format!("resp_mock_{}", self.responses);
        if !self.send(json!({ "type": "response.created", "response": { "id": id, "status": "in_progress" } })).await {
            return false;
        }
        if hold {
            self.held = Some(id);
            return true;
        }
        let delta = json!({ "type": "response.output_text.delta", "response_id": id, "delta": "Hello from the mock" });
        self.send(delta).await
            && self
                .send(json!({ "type": "response.done", "response": {
                    "id": id, "object": "realtime.response", "status": "completed", "usage": usage(),
                }}))
                .await
    }
}

async fn session(socket: WebSocket, model: String) {
    let mut s = Script { socket, events: 0, responses: 0, held: None };
    let created = json!({ "type": "session.created", "session": { "id": "sess_mock", "model": model } });
    if !s.send(created).await {
        return;
    }
    while let Some(Ok(message)) = s.socket.recv().await {
        let Message::Text(text) = message else {
            if matches!(message, Message::Close(_)) {
                break;
            }
            continue;
        };
        let v: Value = serde_json::from_str(&text).unwrap_or_default();
        let ok = match v["type"].as_str().unwrap_or_default() {
            "session.update" => s.send(json!({ "type": "session.updated", "session": v["session"] })).await,
            "response.create" => s.respond(v["response"]["metadata"]["mock"] == "hold").await,
            "input_audio_buffer.commit" => {
                s.send(json!({ "type": "input_audio_buffer.committed" })).await && s.respond(false).await
            }
            "response.cancel" => match s.held.take() {
                Some(id) => {
                    let partial = json!({ "total_tokens": 30, "input_tokens": 20, "output_tokens": 10 });
                    s.send(json!({ "type": "response.done", "response": {
                        "id": id, "object": "realtime.response", "status": "cancelled",
                        "status_details": { "type": "cancelled", "reason": "client_cancelled" }, "usage": partial,
                    }}))
                    .await
                }
                None => true,
            },
            _ => true,
        };
        if !ok {
            break;
        }
    }
}
