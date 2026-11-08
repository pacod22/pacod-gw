//! Remote agents through `/a2a/{name}` (runbook section 21.4): the card as approved and
//! rewritten, messages and tasks passed on, each workspace's tasks kept apart, what is refused,
//! streams, a changed card held, and a record of every request.

mod common;

use common::{Fixture, IngestSink, TestGw};
use pacod_gw_testkit::a2a::MockAgent;
use serde_json::{Value, json};

struct Setup {
    gw: TestGw,
    fx: Fixture,
    mock: MockAgent,
    sink: IngestSink,
    key: String,
    base: String,
}

async fn grant(gw: &TestGw, org: &str, ws: &str) -> String {
    let (st, v) = gw
        .api(
            "POST",
            &format!("/orgs/{org}/workspaces/{ws}/tool-bundles"),
            Some(json!({ "name": "agents", "allow": ["a2a:researcher"] })),
        )
        .await;
    assert_eq!(st, 201, "{v}");
    let (st, k) = gw
        .api(
            "POST",
            &format!("/orgs/{org}/workspaces/{ws}/keys"),
            Some(json!({ "name": "caller", "tool_bundles": ["agents"] })),
        )
        .await;
    assert_eq!(st, 201, "{k}");
    k["secret"].as_str().unwrap().to_owned()
}

async fn setup() -> Setup {
    let mock = MockAgent::spawn().await;
    mock.require("x-agent-key", "ak-1");
    let sink = IngestSink::start().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", "http://127.0.0.1:9/v1").await;
    let (st, agent) = gw
        .api(
            "POST",
            &format!("/orgs/{}/remote-agents", fx.org),
            Some(json!({ "name": "researcher", "card_url": mock.card_url(), "auth": { "header": "x-agent-key", "prefix": "" } })),
        )
        .await;
    assert_eq!(st, 201, "{agent}");
    let base = format!("/orgs/{}/remote-agents/{}", fx.org, agent["id"].as_str().unwrap());
    gw.api("POST", &format!("{base}/credentials"), Some(json!({ "secret": "ak-1" }))).await;
    let (_, view) = gw.api("POST", &format!("{base}/probe"), None).await;
    assert_eq!(view["status"], "new", "{view}");
    let hash = view["seen"]["hash"].as_str().unwrap().to_owned();
    let (st, v) = gw.api("POST", &format!("{base}/card/approve"), Some(json!({ "hash": hash }))).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["status"], "approved");
    let key = grant(&gw, &fx.org, &fx.ws).await;
    Setup { gw, fx, mock, sink, key, base }
}

async fn rpc(s: &Setup, key: &str, method: &str, params: Value) -> (u16, Value) {
    let resp =
        s.gw.http
            .post(format!("{}/a2a/researcher", s.gw.data))
            .bearer_auth(key)
            .header("a2a-version", "1.0")
            .json(&json!({ "jsonrpc": "2.0", "id": 5, "method": method, "params": params }))
            .send()
            .await
            .unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or_default())
}

fn message(text: &str, extra: Value) -> Value {
    let mut m = json!({ "messageId": "m-1", "role": "ROLE_USER", "parts": [{ "text": text }] });
    for (k, v) in extra.as_object().unwrap() {
        m[k] = v.clone();
    }
    json!({ "message": m })
}

#[tokio::test]
async fn the_card_is_served_as_approved_and_pointed_at_the_gateway() {
    let s = setup().await;
    let url = format!("{}/a2a/researcher/.well-known/agent-card.json", s.gw.data);
    let card: Value = s.gw.http.get(&url).bearer_auth(&s.key).send().await.unwrap().json().await.unwrap();
    let iface = &card["supportedInterfaces"];
    assert_eq!(iface.as_array().unwrap().len(), 1, "{card}");
    assert!(iface[0]["url"].as_str().unwrap().ends_with("/a2a/researcher"), "{card}");
    assert_eq!(
        (iface[0]["protocolBinding"].as_str(), iface[0]["protocolVersion"].as_str()),
        (Some("JSONRPC"), Some("1.0"))
    );
    assert!(card.get("signatures").is_none(), "the agent's signatures cannot survive the rewriting");
    assert!(card["securitySchemes"]["gateway"].is_object() && card["securitySchemes"].get("key").is_none());
    assert_eq!(card["capabilities"]["pushNotifications"], false);
    assert_eq!(card["skills"][0]["id"], "search");
    assert!(!card.to_string().contains("acme"), "the agent's tenant is not the caller's business");
    // A key, and a grant: else there is no such agent.
    assert_eq!(s.gw.http.get(&url).send().await.unwrap().status(), 401);
    assert_eq!(s.gw.http.get(&url).bearer_auth(&s.fx.key).send().await.unwrap().status(), 404);
}

#[tokio::test]
async fn tasks_are_passed_on_and_kept_apart_by_workspace() {
    let s = setup().await;
    let (st, sent) = rpc(&s, &s.key, "SendMessage", message("rust gateways", json!({ "contextId": "my-ctx" }))).await;
    assert_eq!(st, 200, "{sent}");
    let task = &sent["result"]["task"];
    let task_id = task["id"].as_str().unwrap().to_owned();
    assert!(task_id.starts_with("gwt1."), "the agent's id is not shown: {sent}");
    assert_eq!(task["contextId"], "my-ctx", "the caller sees its own context");
    assert_eq!(task["history"][0]["taskId"], task_id.as_str());
    assert_eq!(task["artifacts"][0]["parts"][0]["text"], "found: rust gateways");
    // The agent got its own ids, the caller's context sealed for the workspace, its tenant, and
    // the organisation's credential, never the caller's key.
    let at_agent = s.mock.requests().last().cloned().unwrap();
    let ctx = at_agent.body["params"]["message"]["contextId"].as_str().unwrap();
    assert!(ctx.starts_with("gwc1."), "{ctx}");
    assert_eq!(at_agent.body["params"]["tenant"], "acme");
    assert_eq!(at_agent.headers["x-agent-key"], "ak-1");
    assert!(at_agent.headers.get("authorization").is_none());

    let (_, got) = rpc(&s, &s.key, "GetTask", json!({ "id": task_id })).await;
    assert_eq!(got["result"]["id"], task_id.as_str(), "{got}");
    assert_eq!(s.mock.requests().last().unwrap().body["params"]["id"], "task-1");
    let (_, forged) = rpc(&s, &s.key, "GetTask", json!({ "id": "task-1" })).await;
    assert_eq!(forged["error"]["code"], -32001, "an agent's raw id is not taken: {forged}");

    // Another workspace of the organisation, granted the same agent, cannot reach the task.
    let (_, ws2) = s.gw.api("POST", &format!("/orgs/{}/workspaces", s.fx.org), Some(json!({ "name": "other" }))).await;
    let other = grant(&s.gw, &s.fx.org, ws2["id"].as_str().unwrap()).await;
    let (_, theirs) = rpc(&s, &other, "GetTask", json!({ "id": task_id })).await;
    assert_eq!(theirs["error"]["code"], -32001, "{theirs}");
    let (_, cancel) = rpc(&s, &other, "CancelTask", json!({ "id": task_id })).await;
    assert_eq!(cancel["error"]["code"], -32001);
    // The same context id of another workspace is another context at the agent.
    rpc(&s, &other, "SendMessage", message("x", json!({ "contextId": "my-ctx" }))).await;
    let theirs_at_agent =
        s.mock.requests().last().unwrap().body["params"]["message"]["contextId"].as_str().unwrap().to_owned();
    assert_ne!(theirs_at_agent, ctx);
}

#[tokio::test]
async fn what_cannot_be_passed_on_is_refused() {
    let s = setup().await;
    let before = s.mock.requests().len();
    for (method, params, code) in [
        ("ListTasks", json!({}), -32004),
        ("CreateTaskPushNotificationConfig", json!({}), -32003),
        ("GetExtendedAgentCard", json!({}), -32007),
        ("message/send", json!({}), -32601),
    ] {
        let (_, v) = rpc(&s, &s.key, method, params).await;
        assert_eq!(v["error"]["code"], code, "{method}: {v}");
    }
    let mut push = message("x", json!({}));
    push["configuration"] = json!({ "taskPushNotificationConfig": { "url": "http://169.254.169.254/" } });
    let (_, v) = rpc(&s, &s.key, "SendMessage", push).await;
    assert_eq!(v["error"]["code"], -32003, "{v}");
    assert_eq!(s.mock.requests().len(), before, "none of it reached the agent");
    let resp =
        s.gw.http
            .post(format!("{}/a2a/researcher", s.gw.data))
            .bearer_auth(&s.key)
            .header("a2a-version", "0.3")
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "SendMessage", "params": message("x", json!({})) }))
            .send()
            .await
            .unwrap();
    assert_eq!(resp.status(), 400);
    let v: Value = resp.json().await.unwrap();
    assert_eq!(v["error"]["code"], -32009, "{v}");
}

#[tokio::test]
async fn a_stream_is_relayed_with_its_ids_rewritten_and_a_changed_card_is_held() {
    let s = setup().await;
    let resp = s
        .gw
        .http
        .post(format!("{}/a2a/researcher", s.gw.data))
        .bearer_auth(&s.key)
        .json(&json!({ "jsonrpc": "2.0", "id": 8, "method": "SendStreamingMessage", "params": message("stream", json!({})) }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    assert!(resp.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
    let text = resp.text().await.unwrap();
    let events: Vec<Value> =
        text.split("\n\n").filter_map(|e| e.strip_prefix("data: ")).map(|d| serde_json::from_str(d).unwrap()).collect();
    assert_eq!(events.len(), 2, "{text}");
    let id = events[0]["result"]["task"]["id"].as_str().unwrap();
    assert!(id.starts_with("gwt1."), "{text}");
    assert_eq!(events[1]["result"]["statusUpdate"]["taskId"], id);
    assert!(!text.contains("task-1"), "{text}");

    // The agent changes its card: after a probe it is held, and not reached.
    let mut card = s.mock.card();
    card["skills"][0]["description"] = json!("Searches the web and your mailbox.");
    s.mock.set_card(card);
    let (_, view) = s.gw.api("POST", &format!("{}/probe", s.base), None).await;
    assert_eq!(view["status"], "changed", "{view}");
    let (st, _) = rpc(&s, &s.key, "SendMessage", message("x", json!({}))).await;
    assert_eq!(st, 404);

    let records = s.sink.wait_tool_calls(1).await;
    let r = &records[0];
    assert_eq!(
        (r.protocol.as_str(), r.target.as_str(), r.operation.as_str()),
        ("a2a", "researcher", "SendStreamingMessage")
    );
    assert_eq!((r.verdict.as_str(), r.status.as_str(), r.protocol_version.as_str()), ("allowed", "ok", "1.0"));
    assert!(r.pin_hash.starts_with("sha256:") && r.arguments_hmac.starts_with("hmac1:"), "{r:?}");
}
