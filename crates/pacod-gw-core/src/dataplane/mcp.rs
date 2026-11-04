//! `POST /mcp`: pacod-broker's discovery as MCP tools, behind a gateway key (runbook section 20;
//! the gateway's virtual MCP server of catalogue MCP-01 and ING-13).
//!
//! Agents that already hold a gateway key find models, tools and MCP servers with the broker's
//! `discover_resources` and `describe_resource`, without a broker account of their own. The core
//! forwards the JSON-RPC request to the broker's `/mcp` with its own broker token; only the
//! protocol's handshake and these two tools pass. The broker answers with what the core's token
//! may see.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::Gw;
use super::auth::identify;
use super::client_ip::ClientIp;
use super::errors::Envelope;

const TOOLS: [&str; 2] = ["discover_resources", "describe_resource"];
const METHODS: [&str; 5] = ["initialize", "notifications/initialized", "ping", "tools/list", "tools/call"];
/// A discovery request is a query, not a document: anything bigger is not one.
const MAX_BODY: usize = 64 << 10;

fn rpc_error(id: &Value, code: i64, message: &str) -> Response {
    let body = json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } });
    (StatusCode::OK, axum::Json(body)).into_response()
}

/// Whether a request may pass to the broker: the handshake, the tool list, and the two tools.
pub fn allowed(req: &Value) -> Result<(), &'static str> {
    let method = req["method"].as_str().unwrap_or_default();
    if !METHODS.contains(&method) {
        return Err("the gateway's MCP server offers the broker's discovery tools only");
    }
    if method == "tools/call" && !TOOLS.contains(&req["params"]["name"].as_str().unwrap_or_default()) {
        return Err("unknown tool: discover_resources and describe_resource are offered");
    }
    Ok(())
}

fn http() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_default()
    })
}

pub async fn handle(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(e) = identify(&gw, &headers, Some(client)) {
        return e.response(Envelope::Gateway, "");
    }
    let Some(link) = gw.boot.broker.clone() else {
        return (StatusCode::NOT_FOUND, axum::Json(json!({ "error": "this gateway has no broker (PGW_BROKER_URL)" })))
            .into_response();
    };
    if body.len() > MAX_BODY {
        return (StatusCode::PAYLOAD_TOO_LARGE, axum::Json(json!({ "error": "at most 64 KiB" }))).into_response();
    }
    let Ok(req) = serde_json::from_slice::<Value>(&body) else {
        return rpc_error(&Value::Null, -32700, "not JSON");
    };
    if let Err(why) = allowed(&req) {
        return rpc_error(&req["id"], -32601, why);
    }
    metrics::counter!("gw_mcp_requests_total", "method" => req["method"].as_str().unwrap_or_default().to_owned())
        .increment(1);
    let mut r = http().post(format!("{}/mcp", link.url)).header(header::ACCEPT, "application/json").json(&req);
    if let Some(t) = &link.token {
        r = r.bearer_auth(t);
    }
    match r.send().await {
        Ok(resp) => {
            let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let ct = resp.headers().get(header::CONTENT_TYPE).cloned();
            let bytes = resp.bytes().await.unwrap_or_default();
            let mut out = (status, bytes).into_response();
            if let Some(ct) = ct {
                out.headers_mut().insert(header::CONTENT_TYPE, ct);
            }
            out
        }
        Err(e) => {
            tracing::warn!(error = %e, "the broker's MCP server did not answer");
            rpc_error(&req["id"], -32603, "the broker does not answer")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_handshake_and_the_discovery_tools_pass() {
        assert!(allowed(&json!({ "method": "initialize" })).is_ok());
        assert!(allowed(&json!({ "method": "tools/list" })).is_ok());
        assert!(allowed(&json!({ "method": "tools/call", "params": { "name": "discover_resources" } })).is_ok());
        assert!(allowed(&json!({ "method": "tools/call", "params": { "name": "delete_everything" } })).is_err());
        assert!(allowed(&json!({ "method": "resources/read" })).is_err());
    }
}
