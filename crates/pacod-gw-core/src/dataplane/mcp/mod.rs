//! `/mcp`: the gateway's MCP server (catalogue ING-13 and MCP-01; runbook section 20.7).
//!
//! It serves pacod-broker's discovery tools to callers with a gateway key (`broker.rs`). Only
//! `POST` is served: axum answers any other method with `405` and an `Allow` header, which is
//! what both protocol revisions ask of a server that offers no stream of its own.
//!
//! The data plane's router turns axum's body limit off, so that each handler bounds what it
//! reads. This one reads the body only once the key is known, and never more than [`MAX_BODY`].

mod broker;

use std::sync::Arc;

use axum::Json;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use http_body_util::BodyExt;
use serde_json::{Value, json};

use super::Gw;
use super::auth::identify;
use super::client_ip::ClientIp;
use super::errors::Envelope;

/// A discovery request is a query, not a document: anything bigger is not one.
const MAX_BODY: usize = 64 << 10;

/// JSON-RPC's own error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INTERNAL_ERROR: i64 = -32603;

/// An error answer. JSON-RPC errors travel with HTTP 200, as the protocol's transports expect.
fn rpc_error(id: &Value, code: i64, message: &str) -> Response {
    let body = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } });
    (StatusCode::OK, Json(body)).into_response()
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

pub async fn handle(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    if let Err(e) = identify(&gw, &parts.headers, Some(client)) {
        return e.response(Envelope::Gateway, "");
    }
    let Some(link) = gw.boot.broker.clone() else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "this gateway has no broker (PGW_BROKER_URL)" })))
            .into_response();
    };
    let bytes = match http_body_util::Limited::new(body, MAX_BODY).collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({ "error": "at most 64 KiB" }))).into_response(),
    };
    match message(&bytes) {
        Ok(req) => broker::forward(&link, req).await,
        Err((code, why)) => rpc_error(&Value::Null, code, why),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_message_is_taken_and_batches_are_not() {
        assert!(message(br#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).is_ok());
        assert_eq!(message(br#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#).unwrap_err().0, INVALID_REQUEST);
        assert_eq!(message(b"7").unwrap_err().0, INVALID_REQUEST);
        assert_eq!(message(b"{").unwrap_err().0, PARSE_ERROR);
    }
}
