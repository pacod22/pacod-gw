//! The tool servers and the agent that `gw-mock` serves next to the providers, on one port
//! (`MockProvider::bind_with`): both MCP revisions, `wait`, the agent's card at the host the
//! request names, a message's delay, and the control endpoints that change a definition.

use std::time::{Duration, Instant};

use pacod_gw_testkit::mcp::{LEGACY, MODERN};
use pacod_gw_testkit::mock::{MockProvider, tools_and_agents};
use serde_json::{Value, json};

use crate::support::client;

async fn mock(capture_limit: usize) -> (MockProvider, pacod_gw_testkit::mock::ToolsAndAgents) {
    let (extra, mocks) = tools_and_agents(capture_limit);
    (MockProvider::bind_with("127.0.0.1:0", extra).await.unwrap(), mocks)
}

/// A request of the 2026-07-28 revision, with its headers.
async fn modern(base: &str, method: &str, mut params: Value) -> (u16, Value) {
    params["_meta"] = json!({ "io.modelcontextprotocol/protocolVersion": MODERN });
    let mut r =
        client().post(format!("{base}/mcp")).header("mcp-protocol-version", MODERN).header("mcp-method", method);
    if let Some(name) = params["name"].as_str() {
        r = r.header("mcp-name", name);
    }
    let resp = r.json(&json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })).send().await.unwrap();
    (resp.status().as_u16(), resp.json().await.unwrap_or_default())
}

/// Every page of the modern server's tool list.
async fn tool_names(base: &str) -> Vec<String> {
    let (mut names, mut cursor) = (Vec::new(), None::<String>);
    loop {
        let params = cursor.map_or(json!({}), |c| json!({ "cursor": c }));
        let (_, v) = modern(base, "tools/list", params).await;
        names.extend(v["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap().to_owned()));
        match v["result"]["nextCursor"].as_str() {
            Some(c) => cursor = Some(c.to_owned()),
            None => return names,
        }
    }
}

#[tokio::test]
async fn both_revisions_are_served_next_to_the_providers_with_wait() {
    let (mock, mocks) = mock(100).await;
    let base = mock.base_url();
    assert_eq!(tool_names(&base).await, ["echo", "big", "fail", "picture", "repo.search", "wait"]);
    let started = Instant::now();
    let (st, v) =
        modern(&base, "tools/call", json!({ "name": "wait", "arguments": { "ms": 150, "text": "done" } })).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["result"]["content"][0]["text"], "done");
    assert!(started.elapsed() >= Duration::from_millis(150));
    let (_, v) = modern(&base, "tools/call", json!({ "name": "wait", "arguments": { "ms": 60001 } })).await;
    assert_eq!(v["error"]["code"], -32602, "{v}");

    // The 2025-11-25 revision at /mcp/legacy, with its session.
    let http = client();
    let init = http
        .post(format!("{base}/mcp/legacy"))
        .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {} }))
        .send()
        .await
        .unwrap();
    let session = init.headers()["mcp-session-id"].to_str().unwrap().to_owned();
    assert_eq!(init.json::<Value>().await.unwrap()["result"]["protocolVersion"], LEGACY);
    let called: Value = http
        .post(format!("{base}/mcp/legacy"))
        .header("mcp-session-id", &session)
        .json(&json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                       "params": { "name": "wait", "arguments": { "ms": 0, "text": "old" } } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(called["result"]["content"][0]["text"], "old");
    assert_eq!(http.get(format!("{base}/mcp")).send().await.unwrap().status(), 405);

    // Each keeps its own requests, and the provider none of them.
    assert_eq!(mocks.modern.calls().len(), 2);
    assert_eq!(mocks.legacy.requests().len(), 2);
    assert!(mock.captured().is_empty());
    // The providers are served as before.
    let chat = http
        .post(format!("{base}/v1/chat/completions"))
        .json(&json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(chat.status(), 200);
}

#[tokio::test]
async fn a_definition_changes_on_request_and_comes_back() {
    let (mock, mocks) = mock(100).await;
    let base = mock.base_url();
    let http = client();
    let control = format!("{base}/__mock/mcp/modern/tools");
    let echo = |tools: &Value| tools["tools"].as_array().unwrap().iter().find(|t| t["name"] == "echo").cloned();
    let first: Value = http.post(&control).send().await.unwrap().json().await.unwrap();
    let second: Value = http.post(&control).send().await.unwrap().json().await.unwrap();
    let (a, b) = (echo(&first).unwrap(), echo(&second).unwrap());
    assert_ne!(a["description"], b["description"], "every change is another definition");
    assert_ne!(a["description"], "Answers the text it is given.");
    assert_eq!(mocks.modern.tools()[0]["description"], b["description"]);
    assert_eq!(mocks.legacy.tools()[0]["description"], "Answers the text it is given.", "the other is left alone");
    let reset = http.delete(&control).send().await.unwrap();
    assert_eq!(reset.status(), 200);
    assert_eq!(echo(&reset.json().await.unwrap()).unwrap()["description"], "Answers the text it is given.");
    // A list replaces the tools.
    let set: Value = http
        .post(&control)
        .json(&json!([{ "name": "only", "inputSchema": { "type": "object" } }]))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(set["tools"].as_array().unwrap().len(), 1);
    assert_eq!(tool_names(&base).await, ["only"]);
    assert_eq!(http.post(&control).body("[1").send().await.unwrap().status(), 400);
}

#[tokio::test]
async fn the_agents_card_names_the_host_it_was_asked_at_and_messages_wait_when_asked() {
    let (mock, _) = mock(100).await;
    let base = mock.base_url();
    let http = client();
    let card: Value = http
        .get(format!("{base}/.well-known/agent-card.json"))
        .header("host", "gw-mock:9000")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(card["supportedInterfaces"][0]["url"], "http://gw-mock:9000/a2a", "{card}");
    let card: Value =
        http.get(format!("{base}/.well-known/agent-card.json")).send().await.unwrap().json().await.unwrap();
    assert_eq!(card["supportedInterfaces"][0]["url"], format!("{base}/a2a"));

    // A changed card has another description, and the reset brings the first back.
    let control = format!("{base}/__mock/a2a/card");
    let changed: Value = http.post(&control).send().await.unwrap().json().await.unwrap();
    assert_ne!(changed["description"], card["description"]);
    let back: Value = http.delete(&control).send().await.unwrap().json().await.unwrap();
    assert_eq!(back, card);

    let send = |metadata: Value| {
        http.post(format!("{base}/a2a")).json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "SendMessage",
            "params": { "message": { "messageId": "m", "role": "ROLE_USER", "parts": [{ "text": "x" }] },
                        "metadata": metadata } }))
    };
    let started = Instant::now();
    let v: Value = send(json!({ "mock/delay_ms": 150 })).send().await.unwrap().json().await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(150));
    let task = v["result"]["task"]["id"].as_str().unwrap().to_owned();
    let got: Value = http
        .post(format!("{base}/a2a"))
        .json(&json!({ "jsonrpc": "2.0", "id": 2, "method": "GetTask", "params": { "id": task } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(got["result"]["id"], task.as_str(), "{got}");
}

#[tokio::test]
async fn under_load_nothing_is_kept_when_capture_is_off() {
    let (mock, mocks) = mock(0).await;
    let base = mock.base_url();
    for _ in 0..3 {
        modern(&base, "tools/call", json!({ "name": "echo", "arguments": { "text": "x" } })).await;
    }
    assert!(mocks.modern.requests().is_empty());
}
