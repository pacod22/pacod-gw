//! Agents as the gateway knows them (runbook section 21.3): registered in a workspace, spoken
//! for only by their own keys, narrower than those keys, stopped by one switch, and limited
//! one by one.

mod common;

use common::{Fixture, IngestSink, TestGw};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

struct Setup {
    gw: TestGw,
    fx: Fixture,
    sink: IngestSink,
    _mock: MockProvider,
}

async fn setup() -> Setup {
    let mock = MockProvider::spawn().await;
    let sink = IngestSink::start().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    for agent in [json!({ "name": "planner" }), json!({ "name": "triage", "allowed_models": ["openai/m"] })] {
        let (st, v) = gw.api("POST", &format!("/orgs/{}/workspaces/{}/agents", fx.org, fx.ws), Some(agent)).await;
        assert_eq!(st, 201, "{v}");
    }
    Setup { gw, fx, sink, _mock: mock }
}

/// A chat call; `agent` goes in `x-gw-agent-id`, `body_agent` in the body's `gateway` object.
async fn chat(s: &Setup, key: &str, model: &str, agent: Option<&str>, body_agent: Option<&str>) -> (u16, Value) {
    let mut body = json!({ "model": model, "messages": [{ "role": "user", "content": "hi" }] });
    if let Some(a) = body_agent {
        body["gateway"] = json!({ "agent_id": a });
    }
    let mut r = s.gw.http.post(format!("{}/v1/chat/completions", s.gw.data)).bearer_auth(key).json(&body);
    if let Some(a) = agent {
        r = r.header("x-gw-agent-id", a);
    }
    let resp = r.send().await.unwrap();
    let status = resp.status().as_u16();
    (status, resp.json().await.unwrap_or_default())
}

#[tokio::test]
async fn a_key_speaks_for_its_agents_and_no_other_key_does() {
    let s = setup().await;
    let bound = s.gw.key(&s.fx, json!({ "name": "planner-key", "agents": ["planner"] })).await;
    let runtime = s.gw.key(&s.fx, json!({ "name": "runtime", "agents": ["planner", "triage"] })).await;

    // Bound to one agent: every call is that agent's, named or not.
    assert_eq!(chat(&s, &bound, "openai/m", None, None).await.0, 200);
    assert_eq!(chat(&s, &bound, "openai/m", Some("planner"), None).await.0, 200);
    let (st, v) = chat(&s, &bound, "openai/m", Some("triage"), None).await;
    assert_eq!(st, 403, "{v}");
    let (st, v) = chat(&s, &bound, "openai/m", None, Some("triage")).await;
    assert_eq!(st, 403, "the body says another agent: {v}");

    // Speaking for several: the call names one of them.
    assert_eq!(chat(&s, &runtime, "openai/m", None, None).await.0, 403, "which one?");
    assert_eq!(chat(&s, &runtime, "openai/m", Some("triage"), None).await.0, 200);
    assert_eq!(chat(&s, &runtime, "openai/m", Some("someone"), None).await.0, 403);

    // A key that speaks for no agent cannot claim a registered one; a label of its own passes.
    assert_eq!(chat(&s, &s.fx.key, "openai/m", Some("planner"), None).await.0, 403);
    assert_eq!(chat(&s, &s.fx.key, "openai/m", None, Some("planner")).await.0, 403);
    assert_eq!(chat(&s, &s.fx.key, "openai/m", Some("free-label"), None).await.0, 200);

    // A workspace that accepts registered agents only refuses the label.
    let ws_path = format!("/orgs/{}/workspaces/{}", s.fx.org, s.fx.ws);
    let (_, mut ws) = s.gw.api("GET", &ws_path, None).await;
    ws["settings"]["agent_labels"] = json!("registered_only");
    let (st, v) = s.gw.api("PUT", &ws_path, Some(ws)).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(chat(&s, &s.fx.key, "openai/m", Some("free-label"), None).await.0, 403);
    assert_eq!(chat(&s, &s.fx.key, "openai/m", None, None).await.0, 200, "no agent at all still passes");

    // The usage events carry the agent the gateway decided, not what a caller said.
    let events = s.sink.wait_events(6).await;
    let agents: Vec<&str> = events.iter().filter(|e| e.status == "ok").map(|e| e.agent_id.as_str()).collect();
    assert_eq!(agents, ["planner", "planner", "triage", "free-label", ""], "{agents:?}");
}

#[tokio::test]
async fn an_agent_is_narrower_than_its_key_and_stops_at_one_switch() {
    let s = setup().await;
    let triage = s.gw.key(&s.fx, json!({ "name": "triage-key", "agents": ["triage"] })).await;
    // The key may use every model; the agent only openai/m.
    assert_eq!(chat(&s, &triage, "openai/m", None, None).await.0, 200);
    let (st, v) = chat(&s, &triage, "openai/gpt-test", None, None).await;
    assert_eq!(st, 403, "{v}");

    // Disabled, the agent's calls stop, on every path.
    let agents = format!("/orgs/{}/workspaces/{}/agents", s.fx.org, s.fx.ws);
    let (_, list) = s.gw.api("GET", &agents, None).await;
    let id = list["items"].as_array().unwrap().iter().find(|a| a["name"] == "triage").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (st, v) =
        s.gw.api("PUT", &format!("{agents}/{id}"), Some(json!({ "name": "triage", "status": "disabled" }))).await;
    assert_eq!(st, 200, "{v}");
    let (st, v) = chat(&s, &triage, "openai/m", None, None).await;
    assert_eq!(st, 403, "{v}");
    assert!(v.to_string().contains("disabled"), "{v}");
    let proxied =
        s.gw.http
            .post(format!("{}/proxy/openai/v1/chat/completions", s.gw.data))
            .bearer_auth(&triage)
            .json(&json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }] }))
            .send()
            .await
            .unwrap();
    assert_eq!(proxied.status(), 403, "proxy mode too");
    let mcp =
        s.gw.http
            .post(format!("{}/mcp", s.gw.data))
            .bearer_auth(&triage)
            .json(&json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" }))
            .send()
            .await
            .unwrap();
    assert_eq!(mcp.status(), 403, "and /mcp");
    // Still registered while disabled: another key cannot take its name meanwhile.
    assert_eq!(chat(&s, &s.fx.key, "openai/m", Some("triage"), None).await.0, 403);
    // A key that speaks for it keeps it from being deleted.
    let (st, v) = s.gw.api("DELETE", &format!("{agents}/{id}"), None).await;
    assert_eq!(st, 409, "{v}");
}

#[tokio::test]
async fn each_agent_has_limits_of_its_own() {
    let s = setup().await;
    let planner = s.gw.key(&s.fx, json!({ "name": "p", "agents": ["planner"] })).await;
    let triage = s.gw.key(&s.fx, json!({ "name": "t", "agents": ["triage"] })).await;
    let (st, v) =
        s.gw.api(
            "POST",
            &format!("/orgs/{}/workspaces/{}/limits", s.fx.org, s.fx.ws),
            Some(json!({ "name": "per-agent", "subject": "agent", "metric": "requests", "window": "day",
                         "amount": 1.0, "enforcement": "request" })),
        )
        .await;
    assert_eq!(st, 201, "{v}");
    assert_eq!(chat(&s, &planner, "openai/m", None, None).await.0, 200);
    assert_eq!(chat(&s, &planner, "openai/m", None, None).await.0, 402, "the planner's own allowance is used");
    assert_eq!(chat(&s, &triage, "openai/m", None, None).await.0, 200, "the triage agent has its own");
    assert_eq!(chat(&s, &s.fx.key, "openai/m", None, None).await.0, 200, "calls of no agent are not counted");
    assert_eq!(chat(&s, &s.fx.key, "openai/m", None, None).await.0, 200);
}
