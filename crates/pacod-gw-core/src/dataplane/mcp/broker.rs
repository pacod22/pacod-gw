//! pacod-broker's discovery as MCP tools, behind a gateway key (runbook section 20.7; the
//! gateway's virtual MCP server of catalogue MCP-01 and ING-13).
//!
//! Agents that already hold a gateway key find models, tools and MCP servers with the broker's
//! `discover_resources` and `describe_resource`, without a broker account of their own. The core
//! forwards the JSON-RPC request to the broker's `/mcp` with its own broker token; only the
//! protocol's handshake and these two tools pass. The broker answers with what the core's token
//! may see.
//!
//! The broker is the operator's own (`PGW_BROKER_URL`), so this client is not held to the
//! egress policy, as the core's other calls to it are not.

use std::sync::OnceLock;
use std::time::Duration;

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::Value;

use super::{INTERNAL_ERROR, METHOD_NOT_FOUND, rpc_error};
use crate::bootstrap::BrokerLink;
use crate::telemetry::metrics;

/// The broker's tools this server passes. The broker offers more (`describe_provider`); a tool
/// list that showed them would offer the caller tools it cannot call.
const TOOLS: [&str; 2] = ["discover_resources", "describe_resource"];
const METHODS: [&str; 5] = ["initialize", "notifications/initialized", "ping", "tools/list", "tools/call"];

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

/// The method as a metric label: one of [`METHODS`], or `other`, so that a caller cannot add
/// series by inventing methods.
fn label(method: &str) -> &'static str {
    METHODS.iter().find(|m| **m == method).copied().unwrap_or("other")
}

/// The broker's tool list with only the tools of [`TOOLS`]. An error answer passes as it is;
/// `None` when the answer is neither, so that an unreadable list is never passed on.
fn only_offered(body: &[u8]) -> Option<Value> {
    let mut v: Value = serde_json::from_slice(body).ok()?;
    if v.get("error").is_some() {
        return Some(v);
    }
    let tools = v.pointer_mut("/result/tools")?.as_array_mut()?;
    tools.retain(|t| t["name"].as_str().is_some_and(|n| TOOLS.contains(&n)));
    Some(v)
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

pub(super) async fn forward(link: &BrokerLink, req: Value) -> Response {
    let method = label(req["method"].as_str().unwrap_or_default());
    if let Err(why) = allowed(&req) {
        metrics::record_mcp(method, "refused");
        return rpc_error(&req["id"], METHOD_NOT_FOUND, why);
    }
    let mut r = http().post(format!("{}/mcp", link.url)).header(header::ACCEPT, "application/json").json(&req);
    if let Some(t) = &link.token {
        r = r.bearer_auth(t);
    }
    let resp = match r.send().await {
        Ok(resp) => resp,
        Err(e) => {
            tracing::warn!(error = %e, "the broker's MCP server did not answer");
            metrics::record_mcp(method, "unavailable");
            return rpc_error(&req["id"], INTERNAL_ERROR, "the broker does not answer");
        }
    };
    let status = StatusCode::from_u16(resp.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    let ct = resp.headers().get(header::CONTENT_TYPE).cloned();
    let bytes = resp.bytes().await.unwrap_or_default();
    if method == "tools/list" && status.is_success() {
        let Some(list) = only_offered(&bytes) else {
            tracing::warn!("the broker's tool list could not be read");
            metrics::record_mcp(method, "unavailable");
            return rpc_error(&req["id"], INTERNAL_ERROR, "the broker's tool list could not be read");
        };
        metrics::record_mcp(method, "answered");
        return (status, axum::Json(list)).into_response();
    }
    metrics::record_mcp(method, "answered");
    let mut out = (status, bytes).into_response();
    if let Some(ct) = ct {
        out.headers_mut().insert(header::CONTENT_TYPE, ct);
    }
    out
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn only_the_handshake_and_the_discovery_tools_pass() {
        assert!(allowed(&json!({ "method": "initialize" })).is_ok());
        assert!(allowed(&json!({ "method": "tools/list" })).is_ok());
        assert!(allowed(&json!({ "method": "tools/call", "params": { "name": "discover_resources" } })).is_ok());
        assert!(allowed(&json!({ "method": "tools/call", "params": { "name": "delete_everything" } })).is_err());
        assert!(allowed(&json!({ "method": "resources/read" })).is_err());
    }

    #[test]
    fn the_tool_list_shows_only_the_tools_that_pass() {
        let answer = json!({ "jsonrpc": "2.0", "id": 1, "result": { "tools": [
            { "name": "discover_resources" }, { "name": "describe_provider" }, { "name": "describe_resource" }
        ] } });
        let list = only_offered(answer.to_string().as_bytes()).unwrap();
        let names: Vec<_> = list["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].clone()).collect();
        assert_eq!(names, [json!("discover_resources"), json!("describe_resource")]);
        // An error passes as it is; an answer that is neither is not passed on.
        let error = json!({ "jsonrpc": "2.0", "id": 1, "error": { "code": -32000, "message": "x" } });
        assert_eq!(only_offered(error.to_string().as_bytes()).unwrap(), error);
        assert!(only_offered(b"<html>").is_none());
        assert!(only_offered(br#"{"result":{}}"#).is_none());
    }

    #[test]
    fn a_method_label_is_one_of_a_closed_set() {
        assert_eq!(label("tools/call"), "tools/call");
        assert_eq!(label("anything/else"), "other");
    }
}
