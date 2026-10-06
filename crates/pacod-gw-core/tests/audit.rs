//! Audit mode (concept section 14.6), end to end: what is recorded, who decides, and what a
//! record never contains.

mod common;

use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_events::audit::AuditPart;
use pacod_gw_testkit::mock::{Behavior, MockProvider};
use serde_json::{Value, json};

async fn setup() -> (TestGw, MockProvider, IngestSink, Fixture) {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("anthropic", &mock.base_url()).await;
    (gw, mock, sink, fx)
}

async fn workspace_audit(gw: &TestGw, fx: &Fixture, audit: Value) {
    let path = format!("/orgs/{}/workspaces/{}", fx.org, fx.ws);
    let (st, v) = gw
        .api(
            "PUT",
            &path,
            Some(json!({ "name": "prod", "tag_keys": ["purpose", "site"], "settings": { "audit": audit } })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
}

/// The body of a part, following a reference to the part it equals.
fn body_of<'a>(part: &'a AuditPart, record: &'a pacod_gw_events::audit::AuditRecord) -> &'a str {
    match part.same_as.as_str() {
        "" => &part.body,
        "client_request" => &record.client_request.body,
        other => {
            let i: usize = other
                .trim_start_matches("provider_responses[")
                .trim_end_matches(']')
                .parse()
                .expect("a response index");
            &record.provider_responses[i].body
        }
    }
}

#[tokio::test]
async fn a_relayed_call_is_recorded_in_full_and_without_credentials() {
    let (gw, mock, sink, fx) = setup().await;
    workspace_audit(&gw, &fx, json!({ "mode": "on", "retention_days": 30 })).await;

    let body = "{\"model\":\"claude-test\",\"max_tokens\":64,\n  \"messages\":[{\"role\":\"user\",\"content\":\"Record  this.\"}]}";
    mock.set_script(Behavior::parse_all(["request_id=req_audit_1"]).unwrap());
    let r = gw
        .http
        .post(format!("{}/proxy/anthropic/v1/messages?beta=true", gw.data))
        .header("x-api-key", &fx.key)
        .header("anthropic-version", "2023-06-01")
        .header("content-type", "application/json")
        .header("x-gw-end-user", "u_1")
        .header("x-gw-session", "s_1")
        .header("x-gw-agent-id", "planner")
        .header("x-gw-user-id", "staff-1")
        .header("x-gw-task", "plan")
        .header("x-gw-complexity", "medium")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let answer = r.text().await.unwrap();

    let record = sink.wait_audit(1).await.remove(0);
    let event = sink.wait_events(1).await.remove(0);
    assert_eq!(event.audit_id, record.audit_id, "the usage event names its audit record");
    assert_eq!(record.event_id, event.id);
    assert_eq!(
        (record.workspace_id.as_str(), record.end_user.as_str(), record.session_id.as_str()),
        (fx.ws.as_str(), "u_1", "s_1")
    );
    assert_eq!((record.mode.as_str(), record.model.as_str()), ("proxy", "anthropic/claude-test"));
    // The caller's attribution reaches both, in proxy mode too.
    for (agent, user, task, complexity) in [
        (&record.agent_id, &record.user_id, &record.task, &record.complexity),
        (&event.agent_id, &event.user_id, &event.task, &event.complexity),
    ] {
        assert_eq!(
            (agent.as_str(), user.as_str(), task.as_str(), complexity.as_str()),
            ("planner", "staff-1", "plan", "medium")
        );
    }
    assert_eq!(record.expires_at, record.timestamp + chrono::Duration::days(30));

    // The four parts. In proxy mode without a template each body exists once.
    let c = &record.client_request;
    assert_eq!(
        (c.method.as_str(), c.url.as_str(), c.body.as_str()),
        ("POST", "/proxy/anthropic/v1/messages?beta=true", body)
    );
    let p = &record.provider_requests[0];
    assert!(p.url.ends_with("/v1/messages?beta=true") && p.url.starts_with("http://127.0.0.1"));
    assert_eq!((p.body_encoding.as_str(), p.same_as.as_str(), p.body.as_str()), ("ref", "client_request", ""));
    let resp = &record.provider_responses[0];
    assert_eq!((resp.status, resp.body.as_str()), (200, answer.as_str()));
    assert!(resp.headers.iter().any(|(n, v)| n == "request-id" && v == "req_audit_1"));
    assert_eq!(record.client_response.same_as, "provider_responses[0]");
    assert_eq!(body_of(&record.client_response, &record), answer);

    // Credentials are the one thing left out.
    let text = serde_json::to_string(&record).unwrap();
    assert!(!text.contains(&fx.key), "the gateway key is not in the record");
    assert!(!text.contains(PROVIDER_SECRET), "nor the provider's");
    assert!(c.headers.iter().any(|(n, v)| n == "x-api-key" && *v == format!("[gateway key {}]", fx.key_id)));
    assert!(p.headers.iter().any(|(n, v)| n == "x-api-key" && v == "[removed]"));

    // Resolution, timeline and outcome.
    assert_eq!(record.resolution["route"], "proxy");
    assert_eq!(record.resolution["model_served"], "anthropic/claude-test");
    assert_eq!(record.resolution["credential"]["own"], false);
    assert!(
        record.timeline["upstream_head_us"].as_u64().unwrap() >= record.timeline["first_attempt_us"].as_u64().unwrap()
    );
    assert_eq!(
        (record.outcome["status"].as_str(), record.outcome["finish_reason"].as_str()),
        (Some("ok"), Some("end_turn"))
    );
    assert!(record.outcome["usage"]["input_tokens"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn a_translated_stream_shows_what_the_gateway_changed() {
    let (gw, mock, sink, fx) = setup().await;
    gw.model(
        &fx,
        "anthropic/claude",
        "claude-sonnet-5-5",
        json!({ "free": true, "capability_preset": "anthropic-adaptive" }),
    )
    .await;
    // The workspace does not record; this template does.
    gw.template(&fx, "debugged", json!({ "model": "anthropic/claude", "max_output_tokens": 300, "audit": "on" })).await;
    let limits = format!("/orgs/{}/workspaces/{}/limits", fx.org, fx.ws);
    gw.create(
        &limits,
        json!({ "name": "daily", "subject": "workspace", "metric": "requests", "window": "day", "amount": 100.0 }),
    )
    .await;

    mock.set_script(Behavior::parse_all(["fail=503x1"]).unwrap());
    let request = json!({ "model": "@debugged", "max_tokens": 9000, "temperature": 0.5, "stream": true,
                          "stream_options": { "include_usage": true }, "messages": [{ "role": "user", "content": "one two three" }] });
    let r = gw
        .http
        .post(format!("{}/v1/chat/completions", gw.data))
        .bearer_auth(&fx.key)
        .json(&request)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let sent_to_client = r.text().await.unwrap();

    let record = sink.wait_audit(1).await.remove(0);
    assert_eq!((record.mode.as_str(), record.template.as_str()), ("managed", "debugged"));
    // As received, and as sent to the provider after the template, the filter and the translation.
    assert_eq!(serde_json::from_str::<Value>(&record.client_request.body).unwrap(), request);
    assert_eq!(record.provider_requests.len(), 2, "one entry per attempt");
    let provider: Value = serde_json::from_str(&record.provider_requests[1].body).unwrap();
    assert_eq!((provider["model"].as_str(), provider["max_tokens"].as_u64()), (Some("claude-sonnet-5-5"), Some(300)));
    assert!(provider.get("temperature").is_none());
    assert_eq!(
        provider["messages"],
        json!([{ "role": "user", "content": [{ "type": "text", "text": "one two three" }] }])
    );
    assert_eq!(record.provider_requests[0].same_as, "", "the provider did not get the client's bytes");
    // The failed attempt, then the stream as it arrived and as it was sent, chunk by chunk.
    assert_eq!((record.provider_responses[0].status, record.provider_responses[1].status), (503, 200));
    assert!(record.provider_responses[1].body.contains("event: message_start"));
    assert!(record.provider_responses[1].chunks.len() > 1);
    assert_eq!(record.client_response.body, sent_to_client);
    assert!(record.client_response.body.ends_with("data: [DONE]\n\n") && !record.client_response.chunks.is_empty());
    assert_eq!(record.client_response.status, 200);

    assert_eq!(record.resolution["route"], "translated");
    assert_eq!(record.resolution["template"], json!({ "name": "debugged", "version": 1 }));
    assert_eq!(record.resolution["dropped_params"], json!(["temperature"]));
    assert!(record.resolution["warnings"][0].as_str().unwrap().contains("lowered from 9000 to 300"));
    assert_eq!(record.resolution["limits"][0]["name"], "daily");
    assert_eq!(record.outcome["attempts"], 2);
    assert!(
        record.timeline["final_attempt_us"].as_u64().unwrap() > record.timeline["first_attempt_us"].as_u64().unwrap()
    );

    // A call without the template is not recorded.
    let r = gw
        .http
        .post(format!("{}/v1/chat/completions", gw.data))
        .bearer_auth(&fx.key)
        .json(&json!({ "model": "anthropic/claude", "max_tokens": 10, "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let ev = sink.wait_events(2).await;
    assert_eq!(ev.iter().filter(|e| !e.audit_id.is_empty()).count(), 1);
    assert_eq!(sink.audit_records().len(), 1);
}

#[tokio::test]
async fn who_switches_audit_mode_on_and_what_a_refusal_looks_like() {
    let (gw, _mock, sink, fx) = setup().await;
    let call = |audit: Option<&'static str>| {
        let (http, url, key) = (gw.http.clone(), format!("{}/v1/messages", gw.data), fx.key.clone());
        async move {
            let mut req = http.post(url).header("x-api-key", key).header("anthropic-version", "2023-06-01");
            if let Some(a) = audit {
                req = req.header("x-gw-audit", a);
            }
            let r = req.json(&json!({ "model": "claude-test", "max_tokens": 16, "messages": [{ "role": "user", "content": "hi" }] })).send().await.unwrap();
            r.status().as_u16()
        }
    };

    // On request: only the calls that ask.
    workspace_audit(&gw, &fx, json!({ "mode": "on_request" })).await;
    assert_eq!(call(None).await, 200);
    assert_eq!(call(Some("true")).await, 200);
    let records = sink.wait_audit(1).await;
    sink.wait_events(2).await;
    assert_eq!(records.len(), 1);

    // A refused call is recorded too: the gateway's own answer is what the client was sent.
    workspace_audit(&gw, &fx, json!({ "mode": "on" })).await;
    let limits = format!("/orgs/{}/workspaces/{}/limits", fx.org, fx.ws);
    gw.create(
        &limits,
        json!({ "name": "none", "subject": "workspace", "metric": "requests", "window": "day", "amount": 0.0 }),
    )
    .await;
    assert_eq!(call(None).await, 402);
    let records = sink.wait_audit(2).await;
    let refused = &records[1];
    assert_eq!((refused.client_response.status, refused.provider_requests.len()), (402, 0));
    assert!(refused.client_response.body.contains("billing_error"), "{}", refused.client_response.body);
    assert_eq!(refused.outcome["error_type"], "quota_exceeded");

    // The organisation can rule audit mode out for all its workspaces.
    let (st, v) = gw
        .api(
            "PUT",
            &format!("/orgs/{}", fx.org),
            Some(json!({ "name": "acme", "settings": { "audit": { "rule": "forbid" } } })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(call(Some("true")).await, 402);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(sink.audit_records().len(), 2, "nothing more was recorded");

    // Settings are checked.
    for bad in [
        json!({ "mode": "on", "sample": 0.0 }),
        json!({ "mode": "on", "sample": 1.5 }),
        json!({ "mode": "on", "retention_days": 0 }),
        json!({ "rule": "force" }),
    ] {
        let path = format!("/orgs/{}/workspaces/{}", fx.org, fx.ws);
        let (st, v) = gw.api("PUT", &path, Some(json!({ "name": "prod", "settings": { "audit": bad } }))).await;
        assert_eq!(st, 422, "{bad}: {v}");
    }
}

#[tokio::test]
async fn a_required_record_that_cannot_be_stored_refuses_the_call() {
    // No analytics service: there is no audit spool to write to.
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let ask = || async {
        let r = gw
            .http
            .post(format!("{}/v1/chat/completions", gw.data))
            .bearer_auth(&fx.key)
            .json(&json!({ "model": "openai/m", "messages": [] }))
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.headers().get("x-gw-error-type").map(|v| v.to_str().unwrap().to_owned()))
    };

    workspace_audit(&gw, &fx, json!({ "mode": "on", "guarantee": "required" })).await;
    assert_eq!(ask().await, (503, Some("audit_unavailable".into())));
    assert!(mock.captured().is_empty(), "the call never reached the provider");

    // Best effort: the call proceeds, and the missing record is counted.
    workspace_audit(&gw, &fx, json!({ "mode": "on" })).await;
    assert_eq!(ask().await.0, 200);
    let metrics =
        gw.http.get(format!("http://{}/metrics", gw.app.metrics_addr)).send().await.unwrap().text().await.unwrap();
    assert!(
        metrics.lines().any(|l| l.starts_with("gw_audit_records_dropped_total") && !l.ends_with(" 0")),
        "the loss is visible"
    );
}
