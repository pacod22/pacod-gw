//! `/mcp`: the gateway's MCP server (catalogue ING-13 and MCP-01; runbook section 20.7).
//!
//! It serves pacod-broker's discovery tools to callers with a gateway key (`broker.rs`). Only
//! `POST` is served: axum answers any other method with `405` and an `Allow` header, which is
//! what both protocol revisions ask of a server that offers no stream of its own.
//!
//! The data plane's router turns axum's body limit off, so that each handler bounds what it
//! reads. This one reads the body only once the key is known, and never more than [`MAX_BODY`].
//!
//! Every `tools/call` leaves a record (`pacod_gw_events::ToolCallRecord`, catalogue MCP-14):
//! who called which tool, a keyed hash of the arguments, what the gateway decided and what came
//! of it. The handshake and the tool list leave none; they are counted in the metrics.

mod broker;

use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
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

/// A discovery request is a query, not a document: anything bigger is not one.
const MAX_BODY: usize = 64 << 10;

/// JSON-RPC's own error codes.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INTERNAL_ERROR: i64 = -32603;

/// An error answer, and its size for the record. JSON-RPC errors travel with HTTP 200, as the
/// protocol's transports expect.
fn rpc_error(id: &Value, code: i64, message: &str) -> (Response, usize) {
    let body = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } });
    let bytes = body.to_string().len();
    ((StatusCode::OK, Json(body)).into_response(), bytes)
}

/// Longest protocol version kept on a record: the revisions are dates.
const MAX_VERSION_CHARS: usize = 32;

/// What the answer to one request needs to know of it, for the record of a tool call.
struct Call<'a> {
    gw: &'a Gw,
    caller: &'a Caller,
    headers: &'a HeaderMap,
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
        r.protocol_version = self
            .headers
            .get("mcp-protocol-version")
            .and_then(|v| v.to_str().ok())
            .or_else(|| req["params"]["_meta"]["io.modelcontextprotocol/protocolVersion"].as_str())
            .filter(|v| v.len() <= MAX_VERSION_CHARS)
            .unwrap_or_default()
            .to_owned();
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
    let started = Instant::now();
    let (parts, body) = req.into_parts();
    let caller = match identify(&gw, &parts.headers, Some(client)) {
        Ok(caller) => caller,
        Err(e) => return e.response(Envelope::Gateway, ""),
    };
    let Some(link) = gw.boot.broker.clone() else {
        return (StatusCode::NOT_FOUND, Json(json!({ "error": "this gateway has no broker (PGW_BROKER_URL)" })))
            .into_response();
    };
    let bytes = match http_body_util::Limited::new(body, MAX_BODY).collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => return (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({ "error": "at most 64 KiB" }))).into_response(),
    };
    let call = Call { gw: &gw, caller: &caller, headers: &parts.headers, started, request_bytes: bytes.len() as u64 };
    match message(&bytes) {
        Ok(req) => broker::forward(&link, req, &call).await,
        Err((code, why)) => rpc_error(&Value::Null, code, why).0,
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
