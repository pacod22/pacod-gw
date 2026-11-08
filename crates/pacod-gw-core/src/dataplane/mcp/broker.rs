//! pacod-broker's discovery as MCP tools (runbook section 20.7; catalogue ING-13).
//!
//! Agents that already hold a gateway key find models, tools and MCP servers with the broker's
//! `discover_resources` and `describe_resource`, without a broker account of their own. Where
//! the gateway has a broker (`PGW_BROKER_URL`), `/mcp` lists these two tools beside the tool
//! servers' and passes their calls to the broker's own `/mcp` with the core's broker token;
//! the broker answers with what that token may see. Nothing of the caller's request but the
//! tool's name and arguments reaches the broker. A broker that does not answer leaves its tools
//! out of the list, not the list out.
//!
//! The broker is the operator's own, so this client is not held to the tool servers' egress
//! list, as the core's other calls to it are not.

use std::sync::OnceLock;
use std::time::Duration;

use axum::http::header;
use serde_json::{Value, json};

use crate::bootstrap::BrokerLink;

/// The broker's tools this server passes. The broker offers more (`describe_provider`); a tool
/// list that showed them would offer the caller tools it cannot call.
pub const TOOLS: [&str; 2] = ["discover_resources", "describe_resource"];
/// The target of the broker's tools on the records of their calls.
pub const TARGET: &str = "broker";
/// What is read of a broker's answer at most.
const MAX_ANSWER: usize = 4 << 20;

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

/// Sends one JSON-RPC request to the broker and returns its answer.
async fn send(link: &BrokerLink, method: &str, params: Value) -> Result<Value, String> {
    let body = json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params });
    let mut r = http().post(format!("{}/mcp", link.url)).header(header::ACCEPT, "application/json").json(&body);
    if let Some(t) = &link.token {
        r = r.bearer_auth(t);
    }
    let resp = r.send().await.map_err(|e| format!("the broker does not answer: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("the broker answered HTTP {}", resp.status().as_u16()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("the broker's answer broke off: {e}"))?;
    if bytes.len() > MAX_ANSWER {
        return Err("the broker's answer is too large".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "the broker's answer is not JSON".into())
}

/// The broker's tools that pass, as the broker lists them; `None` when it does not answer.
pub async fn list(link: &BrokerLink) -> Option<Vec<Value>> {
    match send(link, "tools/list", json!({})).await {
        Ok(answer) => Some(only_offered(&answer)?),
        Err(e) => {
            tracing::warn!(error = %e, "the broker's tool list is left out of /mcp");
            None
        }
    }
}

/// The tools of a broker's tool list that pass; `None` when the answer is not a tool list.
fn only_offered(answer: &Value) -> Option<Vec<Value>> {
    let tools = answer.pointer("/result/tools")?.as_array()?;
    Some(tools.iter().filter(|t| t["name"].as_str().is_some_and(|n| TOOLS.contains(&n))).cloned().collect())
}

/// Calls one of the broker's tools. The answer is the broker's JSON-RPC response.
pub async fn call(link: &BrokerLink, tool: &str, arguments: &Value) -> Result<Value, String> {
    let mut params = json!({ "name": tool });
    if !arguments.is_null() {
        params["arguments"] = arguments.clone();
    }
    send(link, "tools/call", params).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_tool_list_shows_only_the_tools_that_pass() {
        let answer = json!({ "jsonrpc": "2.0", "id": 1, "result": { "tools": [
            { "name": "discover_resources" }, { "name": "describe_provider" }, { "name": "describe_resource" }
        ] } });
        let names: Vec<_> = only_offered(&answer).unwrap().iter().map(|t| t["name"].clone()).collect();
        assert_eq!(names, [json!("discover_resources"), json!("describe_resource")]);
        assert!(only_offered(&json!({ "error": { "code": -32000 } })).is_none());
        assert!(only_offered(&json!({ "result": {} })).is_none());
    }
}
