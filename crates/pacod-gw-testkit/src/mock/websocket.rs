//! WebSocket endpoints: `/__ws/echo`, which answers every message with itself, for proxy
//! mode's byte relay. The handshake is captured like any request, so tests can see which
//! headers reached the provider.

use std::sync::Arc;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};

use super::server::{CapturedRequest, Shared};

/// Records the handshake as a captured request.
pub(crate) fn capture(shared: &Shared, req: &Request) {
    shared.capture(|| CapturedRequest {
        method: req.method().to_string(),
        path: req.uri().path_and_query().map_or_else(|| req.uri().path().to_owned(), |p| p.as_str().to_owned()),
        headers: req
            .headers()
            .iter()
            .map(|(n, v)| (n.as_str().to_owned(), String::from_utf8_lossy(v.as_bytes()).into_owned()))
            .collect(),
        body: Default::default(),
    });
}

/// `GET /__ws/echo`. A protocol the caller offers is accepted, the first of them.
pub(crate) async fn echo(State(shared): State<Arc<Shared>>, req: Request) -> Response {
    capture(&shared, &req);
    let protocol = req
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(',').next())
        .map(|p| p.trim().to_owned());
    let (mut parts, _) = req.into_parts();
    let upgrade =
        match <WebSocketUpgrade as axum::extract::FromRequestParts<()>>::from_request_parts(&mut parts, &()).await {
            Ok(u) => u,
            Err(e) => return e.into_response(),
        };
    let upgrade = match protocol {
        Some(p) => upgrade.protocols([p]),
        None => upgrade,
    };
    upgrade.on_upgrade(echo_session)
}

async fn echo_session(mut socket: WebSocket) {
    while let Some(Ok(message)) = socket.recv().await {
        match message {
            Message::Text(_) | Message::Binary(_) => {
                if socket.send(message).await.is_err() {
                    break;
                }
            }
            Message::Close(_) => break,
            _ => {}
        }
    }
}
