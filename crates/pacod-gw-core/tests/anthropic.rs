//! The Anthropic-compatible interface with an Anthropic model (concept sections 8.3 and
//! 9.4), end to end: nothing may be lost on the way there or back.

mod common;

use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_testkit::mock::{Behavior, MockProvider};
use serde_json::{Value, json};

async fn setup() -> (TestGw, MockProvider, IngestSink, Fixture) {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("anthropic", &mock.base_url()).await;
    (gw, mock, sink, fx)
}

fn script(mock: &MockProvider, entries: &[&str]) {
    mock.set_script(Behavior::parse_all(entries.iter().copied()).expect("script"));
}

struct Reply {
    status: u16,
    headers: reqwest::header::HeaderMap,
    text: String,
}

impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text))
    }
}

async fn messages(gw: &TestGw, key: &str, headers: &[(&str, &str)], body: String) -> Reply {
    let mut req = gw
        .http
        .post(format!("{}/v1/messages", gw.data))
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let r = req.body(body).send().await.expect("request");
    Reply { status: r.status().as_u16(), headers: r.headers().clone(), text: r.text().await.expect("body") }
}

/// Sonnet-like rates: input, output, cache read, cache writes by lifetime (USD per million).
fn rates() -> Value {
    json!({ "currency": "USD", "input": 2.0, "output": 10.0, "cache_read": 0.2, "cache_write_5m": 2.5, "cache_write_1h": 4.0 })
}

#[tokio::test]
async fn a_messages_request_reaches_anthropic_with_its_content_untouched() {
    let (gw, mock, sink, fx) = setup().await;
    gw.priced_model(
        &fx,
        "anthropic/claude-sonnet",
        "claude-sonnet-5-5",
        json!({ "capability_preset": "anthropic-adaptive", "max_output_tokens": 64000 }),
        rates(),
    )
    .await;
    gw.template(&fx, "synthesizer", json!({ "model": "anthropic/claude-sonnet", "max_output_tokens": 2000 })).await;

    // The request of concept appendix B, with spacing a re-serialiser would change, a field
    // the gateway does not know, and a gateway object.
    let body = "{\"model\":\"@synthesizer\",\"max_tokens\":4000,\n       \"thinking\":{\"type\":\"adaptive\",\"display\":\"summarized\"},\n       \"system\":[{\"type\":\"text\",\"text\":\"You are a  careful analyst.\",\n                  \"cache_control\":{\"type\":\"ephemeral\"}}],\n       \"context_management\":{\"edits\":[]},\n       \"metadata\":{\"user_id\":\"u_18342\"},\n       \"gateway\":{\"trace_id\":\"turn_9f3\"},\n       \"messages\":[{\"role\":\"user\",\"content\":\"Compare the two offers.\"}]}";
    script(&mock, &["cache_read=6100,request_id=req_fixed_1"]);
    let r = messages(
        &gw,
        &fx.key,
        &[("anthropic-beta", "context-management-2025-06-27"), ("anthropic-version", "2023-06-01")],
        body.into(),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);

    let c = &mock.captured()[0];
    assert_eq!(c.path, "/v1/messages");
    assert_eq!(
        std::str::from_utf8(&c.body).unwrap(),
        "{\"model\":\"claude-sonnet-5-5\",\"max_tokens\":2000,\n       \"thinking\":{\"type\":\"adaptive\",\"display\":\"summarized\"},\n       \"system\":[{\"type\":\"text\",\"text\":\"You are a  careful analyst.\",\n                  \"cache_control\":{\"type\":\"ephemeral\"}}],\n       \"context_management\":{\"edits\":[]},\n       \"metadata\":{\"user_id\":\"u_18342\"},\"messages\":[{\"role\":\"user\",\"content\":\"Compare the two offers.\"}]}",
        "only the model, the ceiling and the gateway object changed"
    );
    assert_eq!(c.header("x-api-key"), Some(PROVIDER_SECRET));
    assert_eq!(c.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(
        c.header("anthropic-beta"),
        Some("context-management-2025-06-27"),
        "beta features are the caller's to ask for"
    );
    assert!(c.headers.iter().all(|(n, v)| !n.starts_with("x-gw-") && !v.contains(&fx.key)));

    // The answer is Anthropic's own, byte for byte: asked directly, the provider says the same.
    script(&mock, &["cache_read=6100,request_id=req_fixed_1"]);
    let direct = gw
        .http
        .post(format!("{}/v1/messages", mock.base_url()))
        .header("x-api-key", "any")
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .body(c.body.clone())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(r.text, direct);
    let v = r.json();
    assert_eq!(v["type"], "message");
    assert!(v["usage"].get("cost").is_none(), "Anthropic's shape has no place for the cost; it is in a header");

    let (input, output) = (v["usage"]["input_tokens"].as_u64().unwrap(), v["usage"]["output_tokens"].as_u64().unwrap());
    let expected = (input as f64 * 2.0 + 6100.0 * 0.2 + output as f64 * 10.0) / 1e6;
    let cost: f64 = r.header("x-gw-cost").unwrap().strip_suffix(" USD").unwrap().parse().unwrap();
    assert!((cost - expected).abs() < 1e-6, "{cost} vs {expected}");
    assert_eq!(r.header("x-gw-model-served"), Some("anthropic/claude-sonnet"));
    assert_eq!(r.header("x-gw-upstream-request-id"), Some("req_fixed_1"));
    assert_eq!(r.header("request-id"), Some("req_fixed_1"), "Anthropic's request id is passed on");
    assert!(r.header("anthropic-ratelimit-requests-remaining").is_none(), "a shared key's limits are not shown");

    let e = sink.wait_events(1).await.remove(0);
    assert_eq!((e.mode.as_str(), e.interface.as_str(), e.path.as_str()), ("managed", "anthropic", "/v1/messages"));
    assert_eq!((e.template.as_str(), e.model_requested.as_str()), ("synthesizer", "@synthesizer"));
    assert_eq!((e.end_user.as_str(), e.trace_id.as_str()), ("u_18342", "turn_9f3"), "metadata.user_id is the end user");
    assert_eq!((e.usage.input_tokens, e.usage.cache_read_tokens, e.usage.output_tokens), (input, 6100, output));
    assert!((e.cost.unwrap() - expected).abs() < 1e-12);
    assert_eq!((e.finish_reason.as_str(), e.status.as_str()), ("end_turn", "ok"));
}

#[tokio::test]
async fn an_anthropic_stream_is_passed_on_event_for_event() {
    let (gw, mock, sink, fx) = setup().await;
    gw.priced_model(
        &fx,
        "anthropic/claude-sonnet",
        "claude-sonnet-5-5",
        json!({ "capability_preset": "anthropic-adaptive" }),
        rates(),
    )
    .await;
    let body = json!({
        "model": "anthropic/claude-sonnet", "max_tokens": 300, "stream": true,
        "thinking": { "type": "adaptive" },
        "messages": [{ "role": "user", "content": "think, then answer at some length please" }]
    });
    script(&mock, &["thinking,split=11,cache_write=40,request_id=req_s"]);
    let r = messages(&gw, &fx.key, &[], body.to_string()).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(r.header("x-gw-cost").is_none());

    script(&mock, &["thinking,cache_write=40,request_id=req_s"]);
    let direct = gw
        .http
        .post(format!("{}/v1/messages", mock.base_url()))
        .header("x-api-key", "any")
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .body(mock.captured()[0].body.clone())
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(r.text, direct, "thinking deltas, the signature and every other event arrive as Anthropic sent them");
    assert!(r.text.contains("signature_delta") && r.text.contains("event: message_stop"));

    let e = sink.wait_events(1).await.remove(0);
    assert_eq!((e.status.as_str(), e.stream, e.usage_source.as_str()), ("ok", true, "provider"));
    assert_eq!(e.usage.cache_write_tokens, 40);
    let expected = (e.usage.input_tokens as f64 * 2.0 + 40.0 * 2.5 + e.usage.output_tokens as f64 * 10.0) / 1e6;
    assert!((e.cost.unwrap() - expected).abs() < 1e-12, "cache writes have their own rate: {:?}", e.cost);
}

#[tokio::test]
async fn what_a_model_does_not_take_is_never_sent_upstream_to_fail_there() {
    let (gw, mock, _sink, fx) = setup().await;
    gw.model(
        &fx,
        "anthropic/fable",
        "claude-fable-5-1",
        json!({ "free": true, "capability_preset": "anthropic-always-thinking", "max_output_tokens": 32000 }),
    )
    .await;
    gw.model(
        &fx,
        "anthropic/haiku",
        "claude-haiku-4-5",
        json!({ "free": true, "capability_preset": "anthropic-budget" }),
    )
    .await;
    let ask = |body: Value, headers: Vec<(&'static str, &'static str)>| {
        let (gw, key) = (&gw, fx.key.clone());
        async move { messages(gw, &key, &headers, body.to_string()).await }
    };
    let user = json!([{ "role": "user", "content": "hi" }]);

    // The newest models: no sampling parameters, no budget, no forced tool, thinking stays on.
    let r = ask(
        json!({
            "model": "anthropic/fable", "temperature": 0.7, "messages": user,
            "thinking": { "type": "enabled", "budget_tokens": 8000 },
            "tools": [{ "name": "lookup", "description": "d", "input_schema": { "type": "object" } }],
            "tool_choice": { "type": "tool", "name": "lookup" }
        }),
        vec![],
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.header("x-gw-dropped-params"), Some("temperature,reasoning.budget_tokens,tool_choice"));
    let sent = mock.captured()[0].json().unwrap();
    assert_eq!(sent["thinking"], json!({ "type": "adaptive" }));
    assert_eq!(sent["max_tokens"], 32000, "a caller that names no limit gets the model's own");
    assert!(sent.get("temperature").is_none() && sent.get("tool_choice").is_none());
    assert_eq!(sent["tools"][0]["name"], "lookup");

    // The same request from a caller who wants to be told instead.
    let r = ask(
        json!({ "model": "anthropic/fable", "temperature": 0.7, "max_tokens": 10, "messages": user }),
        vec![("x-gw-unsupported-params", "reject")],
    )
    .await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (400, Some("unsupported_feature")));
    let v = r.json();
    assert_eq!((v["type"].as_str(), v["error"]["type"].as_str()), (Some("error"), Some("invalid_request_error")));

    // A partial assistant turn cannot be dropped without changing what is asked.
    let r = ask(
        json!({ "model": "anthropic/fable", "max_tokens": 10, "messages": [
            { "role": "user", "content": "hi" }, { "role": "assistant", "content": "{\"answer\":" }
        ] }),
        vec![],
    )
    .await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (400, Some("unsupported_feature")));
    assert!(r.json()["error"]["message"].as_str().unwrap().contains("partial assistant message"));
    mock.clear();
    let r = ask(
        json!({ "model": "anthropic/haiku", "max_tokens": 10, "messages": [
            { "role": "user", "content": "hi" }, { "role": "assistant", "content": "{" }
        ] }),
        vec![],
    )
    .await;
    assert_eq!(r.status, 200, "an older model continues it: {}", r.text);

    // A budget model takes the budget; adaptive thinking is not sent to it.
    mock.clear();
    let r = ask(
        json!({ "model": "anthropic/haiku", "max_tokens": 5000, "thinking": { "type": "enabled", "budget_tokens": 200 }, "messages": user }),
        vec![],
    )
    .await;
    assert_eq!(r.status, 200);
    assert_eq!(
        mock.captured()[0].json().unwrap()["thinking"],
        json!({ "type": "enabled", "budget_tokens": 1024 }),
        "raised to the model's minimum"
    );
}

#[tokio::test]
async fn limits_errors_and_provider_tools_speak_anthropic() {
    let (gw, mock, sink, fx) = setup().await;
    let limits = format!("/orgs/{}/workspaces/{}/limits", fx.org, fx.ws);
    gw.create(
        &limits,
        json!({ "name": "user-requests", "subject": "end_user", "metric": "requests", "window": "day", "amount": 1.0 }),
    )
    .await;
    let ask = |body: Value| {
        let (gw, key) = (&gw, fx.key.clone());
        async move { messages(gw, &key, &[], body.to_string()).await }
    };
    let base = |extra: Value| {
        let mut b =
            json!({ "model": "claude-test", "max_tokens": 16, "messages": [{ "role": "user", "content": "hi" }] });
        b.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        b
    };

    // The provider's own model id works when exactly one granted model has it.
    let r = ask(base(json!({ "metadata": { "user_id": "u_1" } }))).await;
    assert_eq!((r.status, r.header("x-gw-model-served")), (200, Some("anthropic/claude-test")));
    assert!(r.header("x-gw-quota-remaining").unwrap().contains("user-requests=0"));

    // A used-up quota is Anthropic's billing_error, which its SDK does not retry.
    let r = ask(base(json!({ "metadata": { "user_id": "u_1" } }))).await;
    assert_eq!(
        (r.status, r.header("x-gw-error-type"), r.header("x-should-retry")),
        (402, Some("quota_exceeded"), Some("false"))
    );
    let v = r.json();
    assert_eq!((v["type"].as_str(), v["error"]["type"].as_str()), (Some("error"), Some("billing_error")));
    assert!(v["request_id"].as_str().unwrap().starts_with("gw_"));

    // Anthropic's own "overloaded" passes as it is; its other failures are the gateway's 502.
    script(&mock, &["fail=529x9,request_id=req_o"]);
    let r = ask(base(json!({}))).await;
    assert_eq!(r.status, 529);
    assert_eq!(r.json()["error"]["type"], "overloaded_error");
    assert_eq!(r.header("x-gw-attempts"), Some("2"), "it was retried first");
    mock.set_script([]);
    script(&mock, &["fail=500x9"]);
    let r = ask(base(json!({}))).await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (502, Some("upstream_error")));
    assert_eq!(r.json()["error"]["type"], "api_error");
    mock.set_script([]);

    // A workspace can be restricted to some of Anthropic's own tools.
    let ws = format!("/orgs/{}/workspaces/{}", fx.org, fx.ws);
    let (st, v) = gw
        .api("PUT", &ws, Some(json!({ "name": "prod", "tag_keys": ["purpose", "site"], "settings": { "provider_tools": ["web_search"] } })))
        .await;
    assert_eq!(st, 200, "{v}");
    mock.clear();
    let tools = |kind: &str| {
        json!({ "tools": [
        { "type": kind, "name": "t" },
        { "name": "mine", "description": "a tool of the caller", "input_schema": { "type": "object" } }
    ] })
    };
    let r = ask(base(tools("web_search_20260209"))).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(
        mock.captured()[0].json().unwrap()["tools"][0]["type"],
        "web_search_20260209",
        "passed by name, whatever the version"
    );
    let r = ask(base(tools("code_execution_20250825"))).await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (403, Some("not_allowed")));
    assert_eq!(r.json()["error"]["type"], "permission_error");
    // The MCP connector makes the provider reach a server of the caller's choosing.
    let r = ask(base(json!({ "mcp_servers": [{ "type": "url", "url": "https://mcp.example", "name": "x" }] }))).await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (403, Some("not_allowed")));
    assert!(r.json()["error"]["message"].as_str().unwrap().contains("\"mcp\""), "{}", r.text);
    assert_eq!(mock.captured().len(), 1, "refused before the provider");

    let ev = sink.wait_events(6).await;
    assert!(ev.iter().all(|e| e.interface == "anthropic"));
    assert_eq!(ev.iter().filter(|e| e.status == "rejected").count(), 3);
}
