//! Managed mode on the OpenAI-compatible interface (concept sections 4.3 and 9.3), end to
//! end: a gateway in process, a scripted mock provider, a real PostgreSQL.

mod common;

use std::time::Duration;

use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_testkit::mock::{Behavior, MockProvider};
use serde_json::{Value, json};

async fn setup() -> (TestGw, MockProvider, IngestSink, Fixture) {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    (gw, mock, sink, fx)
}

fn script(mock: &MockProvider, entries: &[&str]) {
    mock.set_script(Behavior::parse_all(entries.iter().copied()).expect("script"));
}

/// The rates of the concept's worked example (section 9.2): USD per million tokens.
fn example_rates() -> Value {
    json!({ "currency": "USD", "input": 2.0, "output": 10.0, "cache_read": 0.2 })
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

async fn chat_raw(gw: &TestGw, key: &str, headers: &[(&str, &str)], body: String) -> Reply {
    let mut req = gw
        .http
        .post(format!("{}/v1/chat/completions", gw.data))
        .bearer_auth(key)
        .header("content-type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let r = req.body(body).send().await.expect("request");
    Reply { status: r.status().as_u16(), headers: r.headers().clone(), text: r.text().await.expect("body") }
}

async fn chat(gw: &TestGw, key: &str, body: Value) -> Reply {
    chat_raw(gw, key, &[], body.to_string()).await
}

#[tokio::test]
async fn content_is_forwarded_unchanged_and_the_cost_example_holds() {
    let (gw, mock, sink, fx) = setup().await;
    gw.priced_model(
        &fx,
        "openai/gpt-x",
        "gpt-x-2026-01",
        json!({ "capability_preset": "openai-chat" }),
        example_rates(),
    )
    .await;

    // Sized so that the mock reports the concept's figures: 812 fresh input tokens, 6100 read
    // from the cache, 420 output tokens.
    let system = "s".repeat(1574);
    let user = "u".repeat(1674);
    script(&mock, &["cache_read=6100"]);
    let body = format!(
        "{{ \"model\" : \"openai/gpt-x\",\n  \"messages\":[{{\"role\":\"system\",\"content\":\"{system}\"}},{{\"role\":\"user\",\"content\":\"{user}\"}}] ,\"max_tokens\": 4000,\n  \"gateway\": {{\"end_user\":\"u_18342\",\"trace_id\":\"turn_9f3\",\"tags\":{{\"purpose\":\"synthesizer\",\"nope\":\"x\"}}}} }}"
    );
    let r = chat_raw(&gw, &fx.key, &[], body).await;
    assert_eq!(r.status, 200, "{}", r.text);

    // The provider got the provider's model id, no gateway object, and every other byte as sent.
    let c = &mock.captured()[0];
    assert_eq!(c.path, "/v1/chat/completions");
    assert_eq!(
        std::str::from_utf8(&c.body).unwrap(),
        format!(
            "{{ \"model\" : \"gpt-x-2026-01\",\n  \"messages\":[{{\"role\":\"system\",\"content\":\"{system}\"}},{{\"role\":\"user\",\"content\":\"{user}\"}}] ,\"max_tokens\": 4000 }}"
        )
    );
    assert_eq!(c.header("authorization"), Some(format!("Bearer {PROVIDER_SECRET}").as_str()));
    assert!(c.headers.iter().all(|(n, v)| !n.starts_with("x-gw-") && !v.contains(&fx.key)));

    // The answer is the provider's, with the cost added inside usage.
    let v = r.json();
    assert_eq!(v["usage"]["prompt_tokens"], 6912);
    assert_eq!(v["usage"]["prompt_tokens_details"]["cached_tokens"], 6100);
    assert_eq!(v["usage"]["completion_tokens"], 420);
    assert_eq!(v["usage"]["cost"], json!({ "amount": 0.007044, "currency": "USD", "price_version": 1 }));
    assert_eq!(v["choices"][0]["message"]["content"], format!("echo: {user}"));
    assert_eq!(r.header("x-gw-cost"), Some("0.007044 USD"));
    assert_eq!(r.header("x-gw-model-served"), Some("openai/gpt-x"));
    assert_eq!(r.header("x-gw-attempts"), Some("1"));
    assert_eq!(r.header("content-length"), Some(r.text.len().to_string().as_str()));
    assert!(r.header("x-ratelimit-remaining-tokens").is_none(), "a shared key's rate-limit headers are not passed on");
    let delay: f32 = r.header("x-gw-delay-ms").unwrap().parse().unwrap();
    assert!((0.0..250.0).contains(&delay), "gateway delay: {delay}");

    let e = sink.wait_events(1).await.remove(0);
    assert_eq!((e.mode.as_str(), e.interface.as_str(), e.provider.as_str()), ("managed", "openai", "openai"));
    assert_eq!((e.model_requested.as_str(), e.model_served.as_str()), ("openai/gpt-x", "openai/gpt-x"));
    assert_eq!((e.usage.input_tokens, e.usage.cache_read_tokens, e.usage.output_tokens), (812, 6100, 420));
    assert!((e.cost.unwrap() - 0.007044).abs() < 1e-12, "{:?}", e.cost);
    assert_eq!((e.currency.as_str(), e.price_version, e.price_missing), ("USD", 1, false));
    assert_eq!((e.end_user.as_str(), e.trace_id.as_str()), ("u_18342", "turn_9f3"));
    assert_eq!(e.tags.get("purpose").map(String::as_str), Some("synthesizer"));
    assert!(!e.tags.contains_key("nope"), "undeclared tag keys are dropped");
    assert_eq!((e.status.as_str(), e.finish_reason.as_str(), e.stream), ("ok", "stop", false));
    assert!(e.timing.post_ms >= 0.0 && e.timing.gateway_delay_ms < 250.0);

    // The provider's own model id works too, when exactly one granted model has it.
    let r = chat(&gw, &fx.key, json!({ "model": "gpt-x-2026-01", "messages": [{ "role": "user", "content": "hi" }] }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.header("x-gw-model-served"), Some("openai/gpt-x"));
}

#[tokio::test]
async fn streams_pass_event_by_event_with_the_cost_on_the_usage_chunk() {
    let (gw, mock, sink, fx) = setup().await;
    gw.priced_model(&fx, "openai/gpt-x", "gpt-x", json!({ "capability_preset": "openai-chat" }), example_rates()).await;
    let ask = |include_usage: bool| {
        let mut b = json!({ "model": "openai/gpt-x", "stream": true, "messages": [{ "role": "user", "content": "tell me a longer story please" }] });
        if include_usage {
            b["stream_options"] = json!({ "include_usage": true });
        }
        b
    };

    // Events are cut into 7-byte pieces on the way in, so every one spans chunk boundaries.
    script(&mock, &["split=7"]);
    let r = chat(&gw, &fx.key, ask(true)).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(r.header("content-type").unwrap().starts_with("text/event-stream"), "the provider's content type is kept");
    assert!(r.header("x-gw-cost").is_none(), "the cost of a stream is not known when its head is sent");
    let upstream_body = mock.captured()[0].body.clone();
    assert_eq!(mock.captured()[0].json().unwrap()["stream_options"], json!({ "include_usage": true }));

    // What the provider answers to exactly that request, asked directly.
    let direct = gw
        .http
        .post(format!("{}/v1/chat/completions", mock.base_url()))
        .header("content-type", "application/json")
        .body(upstream_body)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    let events = |s: &str| s.split("\n\n").filter(|e| !e.is_empty()).map(str::to_owned).collect::<Vec<_>>();
    let (got, want) = (events(&r.text), events(&direct));
    assert_eq!(got.len(), want.len());
    let mut usage_events = 0;
    for (g, w) in got.iter().zip(&want) {
        if w.contains("\"usage\":{") {
            usage_events += 1;
            let v: Value = serde_json::from_str(g.strip_prefix("data: ").unwrap()).unwrap();
            let u = &v["usage"];
            let expected =
                (u["prompt_tokens"].as_f64().unwrap() * 2.0 + u["completion_tokens"].as_f64().unwrap() * 10.0) / 1e6;
            assert!((u["cost"]["amount"].as_f64().unwrap() - expected).abs() < 1e-12);
            assert_eq!(u["cost"]["currency"], "USD");
            // Apart from the cost, the event is the provider's.
            let mut without = v.clone();
            without["usage"].as_object_mut().unwrap().remove("cost");
            assert_eq!(without, serde_json::from_str::<Value>(w.strip_prefix("data: ").unwrap()).unwrap());
        } else {
            assert_eq!(g, w, "every other event is the provider's bytes");
        }
    }
    assert_eq!(usage_events, 1);
    assert!(r.text.ends_with("data: [DONE]\n\n"));

    // A caller that did not ask for usage does not get the chunk, and is accounted all the same.
    mock.clear();
    let r = chat(&gw, &fx.key, ask(false)).await;
    assert_eq!(r.status, 200);
    assert!(!r.text.contains("\"usage\":{"), "{}", r.text);
    assert!(r.text.contains("echo:") && r.text.ends_with("data: [DONE]\n\n"));
    assert_eq!(
        mock.captured()[0].json().unwrap()["stream_options"]["include_usage"],
        true,
        "the gateway always asks the provider"
    );

    let ev = sink.wait_events(2).await;
    for e in &ev {
        assert_eq!((e.status.as_str(), e.stream, e.usage_source.as_str()), ("ok", true, "provider"));
        assert!(e.usage.output_tokens > 0 && e.cost.unwrap() > 0.0);
        assert!(e.timing.upstream_stream_ms >= 0.0 && e.timing.relay_max_ms < 250.0);
    }
    assert_eq!(ev[0].cost, ev[1].cost, "the same call costs the same either way");
}

#[tokio::test]
async fn parameters_follow_the_template_and_the_models_capabilities() {
    let (gw, mock, sink, fx) = setup().await;
    // A model that takes sampling parameters only at their default, like the concept's
    // @synthesizer example, and one that reasons.
    gw.model(
        &fx,
        "openai/fixed",
        "fixed-1",
        json!({ "free": true, "capability_preset": "openai-compatible-fixed-temperature" }),
    )
    .await;
    gw.model(
        &fx,
        "openai/o-think",
        "o-think",
        json!({ "free": true, "capability_preset": "openai-reasoning", "max_output_tokens": 20000 }),
    )
    .await;
    gw.template(&fx, "synthesizer", json!({ "model": "openai/fixed", "temperature": { "value": 0.3 } })).await;
    gw.template(
        &fx,
        "thinker",
        json!({ "model": "openai/o-think", "max_output_tokens": 3000, "reasoning": { "effort": { "value": "high" } }, "tags": { "purpose": "think" } }),
    )
    .await;

    // The template's default is not accepted by the model: dropped, reported, and the call succeeds.
    let r =
        chat(&gw, &fx.key, json!({ "model": "@synthesizer", "messages": [{ "role": "user", "content": "hi" }] })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.header("x-gw-dropped-params"), Some("temperature"));
    let sent = mock.captured()[0].json().unwrap();
    assert_eq!(sent, json!({ "model": "fixed-1", "messages": [{ "role": "user", "content": "hi" }] }));

    // The caller's own value is dropped the same way, or refused when the caller asks for that.
    let body =
        json!({ "model": "openai/fixed", "temperature": 0.9, "messages": [{ "role": "user", "content": "hi" }] });
    let r = chat(&gw, &fx.key, body.clone()).await;
    assert_eq!((r.status, r.header("x-gw-dropped-params")), (200, Some("temperature")));
    let r = chat_raw(&gw, &fx.key, &[("x-gw-unsupported-params", "reject")], body.to_string()).await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (400, Some("unsupported_feature")));
    assert_eq!(r.json()["error"]["code"], "unsupported_feature");
    assert_eq!(r.header("x-should-retry"), Some("false"));

    // A reasoning model: the limit moves to the field it takes and is capped by the template;
    // an Anthropic-style thinking object and cache markers are accepted and taken out.
    mock.clear();
    let r = chat_raw(
        &gw,
        &fx.key,
        &[],
        r#"{"model":"@thinker","max_tokens":9000,"top_p":0.5,"thinking":{"type":"adaptive"},"messages":[{"role":"system","content":[{"type":"text","text":"static  prefix","cache_control":{"type":"ephemeral"}}]},{"role":"user","content":"go"}]}"#.into(),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.header("x-gw-dropped-params"), Some("top_p,cache_control"));
    assert_eq!(
        std::str::from_utf8(&mock.captured()[0].body).unwrap(),
        r#"{"model":"o-think","messages":[{"role":"system","content":[{"type":"text","text":"static  prefix"}]},{"role":"user","content":"go"}],"max_completion_tokens":3000,"reasoning_effort":"high"}"#
    );

    // Every call above records an event, the refused one too: wait for the thinker's.
    let ev = sink.wait_events(4).await;
    let e = ev.iter().find(|e| e.template == "thinker").unwrap();
    assert_eq!(e.dropped_params, ["top_p", "cache_control"]);
    assert_eq!(e.tags["purpose"], "think");
    assert_eq!(
        (e.model_requested.as_str(), e.model_served.as_str(), e.template_version),
        ("@thinker", "openai/o-think", 1)
    );

    // What the interface cannot do on this model is said, not ignored.
    gw.model(&fx, "openai/no-tools", "no-tools", json!({ "free": true, "capabilities": { "tools": false } })).await;
    let r = chat(
        &gw,
        &fx.key,
        json!({ "model": "openai/no-tools", "messages": [], "tools": [{ "type": "function", "function": { "name": "f" } }] }),
    )
    .await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (400, Some("unsupported_feature")));

    for (body, why) in [
        (json!({ "messages": [] }), "model is required"),
        (json!({ "model": "openai/fixed" }), "messages is required"),
        (json!({ "model": "openai/nothing", "messages": [] }), "does not exist"),
    ] {
        let r = chat(&gw, &fx.key, body).await;
        assert!(r.status == 400 || r.status == 404, "{}", r.text);
        assert!(r.json()["error"]["message"].as_str().unwrap().contains(why), "{}", r.text);
    }
    let r = chat_raw(&gw, &fx.key, &[], r#"{"model":"a","model":"b","messages":[]}"#.into()).await;
    assert_eq!(r.status, 400, "a repeated key is refused: the gateway and the provider could read it differently");
}

#[tokio::test]
async fn limits_are_checked_at_admission_and_settled_with_real_usage() {
    let (gw, _mock, sink, fx) = setup().await;
    gw.priced_model(&fx, "openai/gpt-x", "gpt-x", json!({}), example_rates()).await;
    let limits = format!("/orgs/{}/workspaces/{}/limits", fx.org, fx.ws);
    gw.create(
        &limits,
        json!({ "name": "user-requests", "subject": "end_user", "metric": "requests", "window": "day", "amount": 2.0 }),
    )
    .await;
    let tokens = gw
        .create(&limits, json!({ "name": "user-tokens", "subject": "end_user", "metric": "tokens", "window": "day", "amount": 100000.0, "enforcement": "advisory" }))
        .await
        .id;
    gw.create(
        &limits,
        json!({ "name": "spend", "subject": "workspace", "metric": "spend", "window": "month", "amount": 50.0 }),
    )
    .await;
    gw.create(
        &limits,
        json!({ "name": "throttle", "subject": "key", "metric": "requests", "window": "minute", "amount": 4.0, "filter": { "tags": { "purpose": "bulk" } } }),
    )
    .await;

    let ask = |user: &'static str, tags: &'static str| {
        let (gw, key) = (&gw, fx.key.clone());
        async move {
            chat_raw(
                gw,
                &key,
                &[("x-gw-end-user", user), ("x-gw-tags", tags)],
                json!({ "model": "openai/gpt-x", "messages": [{ "role": "user", "content": "hello there" }] })
                    .to_string(),
            )
            .await
        }
    };

    let first = ask("u_1", "").await;
    assert_eq!(first.status, 200, "{}", first.text);
    // What is left is reported with the answer: one more request, the whole spend budget.
    let remaining = first.header("x-gw-quota-remaining").unwrap().to_owned();
    assert!(remaining.contains("user-requests=1"), "{remaining}");
    assert!(remaining.contains("spend=50"), "{remaining}");
    assert!(!remaining.contains("throttle"), "a limit whose filter does not match is not counted: {remaining}");
    assert_eq!(ask("u_1", "").await.status, 200);

    // The third call of the day is refused with 402, which no SDK retries.
    let refused = ask("u_1", "").await;
    assert_eq!((refused.status, refused.header("x-gw-error-type")), (402, Some("quota_exceeded")));
    assert_eq!(refused.header("x-should-retry"), Some("false"));
    let v = refused.json();
    assert_eq!(
        (v["error"]["type"].as_str(), v["error"]["code"].as_str()),
        (Some("insufficient_quota"), Some("quota_exceeded"))
    );
    assert!(v["error"]["message"].as_str().unwrap().contains("user-requests"));
    // Another end user has an allowance of their own.
    assert_eq!(ask("u_2", "").await.status, 200);

    // The quota endpoint shows the state, per end user, including the advisory limit.
    let quota = |user: &'static str| {
        let (http, url, key) = (gw.http.clone(), format!("{}/gw/v1/quota?end_user={user}", gw.data), fx.key.clone());
        async move { http.get(url).bearer_auth(key).send().await.unwrap().json::<Value>().await.unwrap() }
    };
    let q = quota("u_1").await;
    let by_name =
        |q: &Value, name: &str| q["limits"].as_array().unwrap().iter().find(|l| l["name"] == name).cloned().unwrap();
    let requests = by_name(&q, "user-requests");
    assert_eq!((requests["used"].as_f64(), requests["remaining"].as_f64()), (Some(2.0), Some(0.0)));
    assert!(requests["resets_at"].is_string());
    let ev = sink.wait_events(4).await;
    let u1_tokens: u64 =
        ev.iter().filter(|e| e.end_user == "u_1").map(|e| e.usage.input_tokens + e.usage.output_tokens).sum();
    let t = by_name(&q, "user-tokens");
    assert_eq!(t["used"].as_f64(), Some(u1_tokens as f64), "settled with what the provider reported");
    assert_eq!(t["enforcement"], "advisory");
    let spent: f64 = ev.iter().filter_map(|e| e.cost).sum();
    assert!((by_name(&q, "spend")["used"].as_f64().unwrap() - spent).abs() < 1e-6, "three calls were paid for");
    let rejected = ev.iter().find(|e| e.status == "rejected").expect("the refused call has an event too");
    assert_eq!((rejected.error_type.as_str(), rejected.http_status, rejected.attempts), ("quota_exceeded", 402, 0));
    assert_eq!(rejected.cost, None);

    // An override lifts one end user's allowance; a reset clears the counters.
    let overrides = format!("/orgs/{}/workspaces/{}/end-user-overrides", fx.org, fx.ws);
    let (st, v) = gw.api("GET", &limits, None).await;
    assert_eq!(st, 200);
    let req_limit = v["items"].as_array().unwrap().iter().find(|l| l["name"] == "user-requests").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    gw.create(&overrides, json!({ "limit_id": req_limit, "end_user": "u_1", "amount": 3.0 })).await;
    assert_eq!(ask("u_1", "").await.status, 200, "the override allows a third call");
    assert_eq!(ask("u_1", "").await.status, 402);
    let (st, v) = gw.api("POST", &format!("{limits}/{req_limit}/reset"), Some(json!({ "subject": "u_1" }))).await;
    assert_eq!(st, 204, "{v}");
    assert_eq!(ask("u_1", "").await.status, 200, "after a reset the count starts again");
    let (st, _) = gw.api("POST", &format!("{limits}/{tokens}/reset"), Some(json!({}))).await;
    assert_eq!(st, 422, "an end-user limit is reset for one end user");

    // A per-minute limit is a throttle: 429, with the time to wait.
    for _ in 0..4 {
        assert_eq!(ask("", "purpose=bulk").await.status, 200);
    }
    let throttled = ask("", "purpose=bulk").await;
    assert_eq!((throttled.status, throttled.header("x-gw-error-type")), (429, Some("rate_limited")));
    let wait: u32 = throttled.header("retry-after").unwrap().parse().unwrap();
    assert!((1..=60).contains(&wait));
    assert!(throttled.header("x-should-retry").is_none(), "a throttle may be retried");
    assert_eq!(ask("", "purpose=other").await.status, 200, "calls the filter does not select are not throttled");
}

#[tokio::test]
async fn a_key_can_cap_its_concurrent_calls() {
    let (gw, mock, _sink, fx) = setup().await;
    let key = gw.key(&fx, json!({ "name": "single", "max_concurrency": 1 })).await;
    script(&mock, &["delay=600"]);
    let slow = {
        let (http, url, key) = (gw.http.clone(), format!("{}/v1/chat/completions", gw.data), key.clone());
        tokio::spawn(async move {
            http.post(url)
                .bearer_auth(key)
                .json(&json!({ "model": "openai/m", "messages": [] }))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        })
    };
    tokio::time::sleep(Duration::from_millis(200)).await;
    let second = chat(&gw, &key, json!({ "model": "openai/m", "messages": [] })).await;
    assert_eq!((second.status, second.header("x-gw-error-type")), (429, Some("rate_limited")));
    assert_eq!(second.header("retry-after"), Some("1"));
    assert_eq!(slow.await.unwrap(), 200);
    // The slot is free again once the first call has ended.
    assert_eq!(chat(&gw, &key, json!({ "model": "openai/m", "messages": [] })).await.status, 200);
}

#[tokio::test]
async fn retries_happen_before_the_first_byte_and_never_after_a_timeout() {
    let (gw, mock, sink, fx) = setup().await;
    gw.template(&fx, "quick", json!({ "model": "openai/m", "timeout_seconds": 1, "retry_max_attempts": 3 })).await;
    let ask = |model: &'static str| {
        let (gw, key) = (&gw, fx.key.clone());
        async move { chat(gw, &key, json!({ "model": model, "messages": [{ "role": "user", "content": "hi" }] })).await }
    };

    // One failure, then success: the caller sees only the success.
    script(&mock, &["fail=503x1"]);
    let r = ask("openai/m").await;
    assert_eq!((r.status, r.header("x-gw-attempts")), (200, Some("2")), "{}", r.text);
    assert_eq!(mock.captured().len(), 2);

    // The provider keeps failing: after the attempts allowed, a 502 in the gateway's words.
    mock.clear();
    script(&mock, &["fail=500x5"]);
    let r = ask("openai/m").await;
    assert_eq!(
        (r.status, r.header("x-gw-error-type"), r.header("x-gw-attempts")),
        (502, Some("upstream_error"), Some("2"))
    );
    assert!(r.json()["error"]["message"].as_str().unwrap().contains("mock: scripted error 500"));
    assert_eq!(mock.captured().len(), 2, "two attempts is the default");
    mock.clear();
    script(&mock, &["fail=500x5"]);
    assert_eq!(ask("@quick").await.header("x-gw-attempts"), Some("3"), "the template allows three");

    // The provider's own rate limit, after the gateway's retries.
    mock.clear();
    script(&mock, &["fail=429x5,retry_after=1"]);
    let r = ask("openai/m").await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (429, Some("upstream_rate_limited")));
    assert_eq!(r.header("retry-after"), Some("1"));
    assert!(r.header("x-should-retry").is_none());

    // What the provider says about the request itself is passed on as it is, not retried.
    mock.clear();
    script(&mock, &["status=400"]);
    let r = ask("openai/m").await;
    assert_eq!((r.status, r.header("x-gw-attempts")), (400, Some("1")));
    assert_eq!(r.json()["error"]["message"], "mock: scripted error 400");
    assert_eq!(mock.captured().len(), 1);

    // A timeout is never retried: the provider may have done the work.
    mock.clear();
    script(&mock, &["hang"]);
    let started = std::time::Instant::now();
    let r = ask("@quick").await;
    assert_eq!(
        (r.status, r.header("x-gw-error-type"), r.header("x-gw-attempts")),
        (504, Some("upstream_timeout"), Some("1"))
    );
    assert_eq!(r.header("x-should-retry"), Some("false"));
    assert!(started.elapsed() < Duration::from_secs(4), "the template's timeout applied");
    assert_eq!(mock.captured().len(), 1);

    let ev = sink.wait_events(6).await;
    let by = |kind: &str| ev.iter().find(|e| e.error_type == kind).unwrap_or_else(|| panic!("no {kind} event"));
    assert_eq!((by("upstream_timeout").status.as_str(), by("upstream_timeout").attempts), ("timeout", 1));
    assert_eq!(by("upstream_rate_limited").status, "error");
    let first = ev.iter().find(|e| e.status == "ok").unwrap();
    assert_eq!(first.attempts, 2);
    assert!(
        first.timing.retry_ms > 100.0,
        "the failed attempt and the wait are in retry_ms: {}",
        first.timing.retry_ms
    );
    assert!(first.timing.gateway_delay_ms < 100.0, "and not in the gateway delay: {}", first.timing.gateway_delay_ms);
}

#[tokio::test]
async fn a_broken_stream_ends_with_an_error_event() {
    let (gw, mock, sink, fx) = setup().await;
    script(&mock, &["chunks=6,reset_after=2"]);
    let r = chat(
        &gw,
        &fx.key,
        json!({ "model": "openai/m", "stream": true, "messages": [{ "role": "user", "content": "a b c d e f g h" }] }),
    )
    .await;
    assert_eq!(r.status, 200);
    let last = r.text.split("\n\n").filter(|e| !e.is_empty()).last().unwrap();
    let v: Value = serde_json::from_str(last.strip_prefix("data: ").unwrap()).unwrap();
    assert_eq!(v["error"]["code"], "upstream_error", "{}", r.text);
    let e = sink.wait_events(1).await.remove(0);
    assert_eq!((e.status.as_str(), e.error_type.as_str()), ("error", "stream"));
    assert!(e.response_bytes_seen(), "what arrived before the break is accounted");
    let _ = mock;
}

trait Seen {
    fn response_bytes_seen(&self) -> bool;
}

impl Seen for pacod_gw_events::UsageEvent {
    /// No usage chunk arrived, so the tokens are an estimate from the bytes that did.
    fn response_bytes_seen(&self) -> bool {
        self.usage_source == "estimated" && self.usage.output_tokens > 0
    }
}

#[tokio::test]
async fn the_life_cycle_and_the_interface_decide_what_can_be_called() {
    let (gw, mock, _sink, fx) = setup().await;
    let old = gw.model(&fx, "openai/gpt-old", "gpt-old", json!({ "free": true })).await;
    let new = gw.model(&fx, "openai/gpt-new", "gpt-new", json!({ "free": true })).await;
    let draft = gw.draft_model(&fx.provider, "openai/draft", "draft", json!({ "free": true })).await;
    let ask = |model: &'static str, key: String| {
        let gw = &gw;
        async move { chat(gw, &key, json!({ "model": model, "messages": [{ "role": "user", "content": "hi" }] })).await }
    };

    // Retired with a successor: served by the successor, and both names are on the response.
    let (st, _) = gw
        .api("POST", &format!("/models/{old}/transition"), Some(json!({ "to": "retired", "successor_id": new })))
        .await;
    assert_eq!(st, 200);
    let r = ask("openai/gpt-old", fx.key.clone()).await;
    assert_eq!((r.status, r.header("x-gw-model-served")), (200, Some("openai/gpt-new")));
    assert_eq!(mock.captured()[0].json().unwrap()["model"], "gpt-new");

    // A draft does not exist for applications; nor does a model that was not granted.
    assert_eq!(ask("openai/draft", fx.key.clone()).await.status, 404);
    let ungranted = gw.draft_model(&fx.provider, "openai/private", "private", json!({ "free": true })).await;
    gw.activate(&ungranted).await;
    let r = ask("openai/private", fx.key.clone()).await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (404, Some("model_not_found")));
    let _ = draft;

    // A key's allow-list and its modes.
    let narrow = gw.key(&fx, json!({ "name": "narrow", "allowed_models": ["openai/m"] })).await;
    assert_eq!(ask("openai/gpt-new", narrow.clone()).await.status, 403);
    assert_eq!(ask("openai/m", narrow).await.status, 200);
    let proxy_only = gw.key(&fx, json!({ "name": "proxy-only", "allowed_modes": ["proxy"] })).await;
    let r = ask("openai/m", proxy_only).await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (403, Some("not_allowed")));

    // A workspace with grants of its own sees only those.
    let grants = format!("/orgs/{}/workspaces/{}/grants", fx.org, fx.ws);
    gw.create(&grants, json!({ "model_id": "openai/m", "max_output_tokens": 50 })).await;
    assert_eq!(ask("openai/gpt-new", fx.key.clone()).await.status, 404, "not among the workspace's own grants");
    mock.clear();
    let r = chat(&gw, &fx.key, json!({ "model": "openai/m", "max_tokens": 4000, "messages": [] })).await;
    assert_eq!(r.status, 200);
    assert_eq!(mock.captured()[0].json().unwrap()["max_tokens"], 50, "the grant's ceiling applies");

    // The model list is exactly what the key can put into `model`.
    gw.template(&fx, "writer", json!({ "model": "openai/m" })).await;
    let list = gw
        .http
        .get(format!("{}/v1/models", gw.data))
        .bearer_auth(&fx.key)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    let ids: Vec<&str> = list["data"].as_array().unwrap().iter().map(|m| m["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["openai/m", "@writer"]);
    assert_eq!(list["data"][1]["gateway"], json!({ "kind": "template", "model": "openai/m", "deprecated": false }));
    let one = gw.http.get(format!("{}/v1/models/openai/m", gw.data)).bearer_auth(&fx.key).send().await.unwrap();
    assert_eq!(one.status(), 200);
    assert_eq!(one.json::<Value>().await.unwrap()["owned_by"], "openai");
    let anthropic = gw
        .http
        .get(format!("{}/v1/models", gw.data))
        .header("x-api-key", &fx.key)
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(anthropic["data"][0]["type"], "model");
    assert_eq!((anthropic["first_id"].as_str(), anthropic["has_more"].as_bool()), (Some("openai/m"), Some(false)));
    let native = gw
        .http
        .get(format!("{}/gw/v1/models", gw.data))
        .bearer_auth(&fx.key)
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(native["models"][0]["max_output_tokens"], 50);
    assert_eq!(native["templates"][0]["id"], "@writer");
    let unknown = gw.http.get(format!("{}/v1/models", gw.data)).bearer_auth("pgw_nope_nope").send().await.unwrap();
    assert_eq!(unknown.status(), 401);
}

#[tokio::test]
async fn a_provider_quirk_profile_shapes_the_request() {
    let (gw, mock, _sink, fx) = setup().await;
    // A second endpoint with Gemini's manners: alternating turns, no cache markers.
    let gemini = gw
        .create(
            "/providers",
            json!({ "name": "gemini", "family": "gemini", "base_url": format!("{}/v1", mock.base_url()), "quirks": { "normalise_history": true } }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{gemini}/credentials"), json!({ "secret": "g-secret" })).await;
    let m = gw.draft_model(&gemini, "gemini/flash", "gemini-flash", json!({ "free": true })).await;
    gw.activate(&m).await;
    gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": m })).await;

    let r = chat(
        &gw,
        &fx.key,
        json!({ "model": "gemini/flash", "messages": [
            { "role": "assistant", "content": "leftover" },
            { "role": "user", "content": "one" },
            { "role": "user", "content": "two" }
        ] }),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let c = &mock.captured()[0];
    assert_eq!(c.json().unwrap()["messages"], json!([{ "role": "user", "content": "one\n\ntwo" }]));
    assert_eq!(c.header("authorization"), Some("Bearer g-secret"), "the compatible endpoint takes a Bearer token");

    // A history that is already acceptable keeps its bytes.
    mock.clear();
    let body = "{\"model\":\"gemini/flash\",\"messages\":[{\"role\":\"user\",\"content\":\"one  two\"}]}";
    assert_eq!(chat_raw(&gw, &fx.key, &[], body.into()).await.status, 200);
    assert_eq!(
        std::str::from_utf8(&mock.captured()[0].body).unwrap(),
        "{\"model\":\"gemini-flash\",\"messages\":[{\"role\":\"user\",\"content\":\"one  two\"}]}"
    );

    // Anthropic models need translation, which this interface does not do without it.
    let (st, presets) = gw.api("GET", "/provider-presets", None).await;
    assert_eq!(st, 200);
    assert_eq!(presets.as_array().unwrap().len(), 15);
    assert!(
        presets
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "glm" && p["quirks"]["reasoning_param"] == "thinking_toggle")
    );
}

#[tokio::test]
async fn the_model_and_tier_the_provider_reports_decide_the_price() {
    let (gw, mock, sink, fx) = setup().await;
    gw.priced_model(
        &fx,
        "openai/big",
        "big",
        json!({}),
        json!({ "currency": "USD", "input": 10.0, "output": 10.0,
                "variants": { "priority": { "input": 20.0, "output": 20.0 } } }),
    )
    .await;
    gw.priced_model(&fx, "openai/small", "small", json!({}), json!({ "currency": "USD", "input": 1.0, "output": 1.0 }))
        .await;
    let ask =
        || chat(&gw, &fx.key, json!({ "model": "openai/big", "messages": [{ "role": "user", "content": "hi" }] }));

    // The provider answered with another model of the repository: it served the call.
    script(&mock, &["model=small"]);
    let r = ask().await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.header("x-gw-model-served"), Some("openai/small"));
    // A dated id of the same model is the same model.
    script(&mock, &["model=big-2026-08-06"]);
    assert_eq!(ask().await.header("x-gw-model-served"), Some("openai/big"));
    // A priority tier is billed at the price's priority rates.
    script(&mock, &["tier=priority"]);
    assert_eq!(ask().await.status, 200);

    let ev = sink.wait_events(3).await;
    let fallback = &ev[0];
    assert_eq!((fallback.model_served.as_str(), fallback.model_reported.as_str()), ("openai/small", "small"));
    let tokens = (fallback.usage.input_tokens + fallback.usage.output_tokens) as f64;
    assert!((fallback.cost.unwrap() - tokens * 1.0 / 1e6).abs() < 1e-12, "at the small model's price");
    assert_eq!((ev[1].model_served.as_str(), ev[1].model_reported.as_str()), ("openai/big", "big-2026-08-06"));
    let priority = &ev[2];
    assert_eq!(priority.price_variant, "priority");
    let tokens = (priority.usage.input_tokens + priority.usage.output_tokens) as f64;
    assert!((priority.cost.unwrap() - tokens * 20.0 / 1e6).abs() < 1e-12, "at the priority rates");
}

#[tokio::test]
async fn a_rate_class_throttles_every_key_that_names_it() {
    let (gw, _mock, _sink, fx) = setup().await;
    gw.create("/rate-classes", json!({ "name": "batch", "requests_per_minute": 2, "max_concurrency": 5 })).await;
    let (st, v) = gw
        .api(
            "POST",
            &format!("/orgs/{}/workspaces/{}/keys", fx.org, fx.ws),
            Some(json!({ "name": "w", "rate_class": "nope" })),
        )
        .await;
    assert_eq!(st, 422, "{v}");
    let key = gw.key(&fx, json!({ "name": "worker", "rate_class": "batch" })).await;
    let ask = || chat(&gw, &key, json!({ "model": "gpt-test", "messages": [{ "role": "user", "content": "hi" }] }));
    assert_eq!(ask().await.status, 200);
    assert_eq!(ask().await.status, 200);
    let r = ask().await;
    assert_eq!((r.status, r.header("x-gw-error-type")), (429, Some("rate_limited")), "{}", r.text);
    // The key's quota shows the class's limit that refused it.
    let quota: Value =
        gw.http.get(format!("{}/gw/v1/quota", gw.data)).bearer_auth(&key).send().await.unwrap().json().await.unwrap();
    let used: Vec<f64> = quota["limits"].as_array().unwrap().iter().filter_map(|l| l["used"].as_f64()).collect();
    assert!(used.contains(&2.0), "{quota}");
    // Another key of the workspace is not in the class.
    assert_eq!(
        chat(&gw, &fx.key, json!({ "model": "gpt-test", "messages": [{ "role": "user", "content": "hi" }] }))
            .await
            .status,
        200
    );
    // A class that keys name cannot be deleted.
    let (_, classes) = gw.api("GET", "/rate-classes", None).await;
    let id = classes["items"][0]["id"].as_str().unwrap().to_owned();
    let (st, v) = gw.api("DELETE", &format!("/rate-classes/{id}"), None).await;
    assert_eq!(st, 409, "{v}");
    assert_eq!(v["error"]["details"]["keys"], json!(["worker"]));
}

#[tokio::test]
async fn every_provider_attempt_is_a_child_span_with_the_providers_request_id() {
    // A collector that keeps what it is sent.
    let received: std::sync::Arc<std::sync::Mutex<Vec<Value>>> = Default::default();
    let kept = received.clone();
    let collector = axum::Router::new().route(
        "/v1/traces",
        axum::routing::post(move |axum::Json(v): axum::Json<Value>| {
            let kept = kept.clone();
            async move {
                kept.lock().unwrap().push(v);
                "{}"
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, collector).await.unwrap() });

    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let db = common::fresh_database().await;
    let gw = TestGw::start_on(&db, Some(&sink), |b| b.otlp_endpoint = Some(format!("http://{addr}"))).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    script(&mock, &["fail=503x1"]);
    let r = chat(&gw, &fx.key, json!({ "model": "openai/m", "messages": [{ "role": "user", "content": "hi" }] })).await;
    assert_eq!(r.status, 200, "{}", r.text);

    let attr = |span: &Value, key: &str| -> Value {
        let a = span["attributes"].as_array().unwrap().iter().find(|a| a["key"] == key).cloned();
        a.map(|a| a["value"].as_object().unwrap().values().next().unwrap().clone()).unwrap_or(Value::Null)
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(8);
    let spans = loop {
        let spans: Vec<Value> = received
            .lock()
            .unwrap()
            .iter()
            .flat_map(|e| e["resourceSpans"][0]["scopeSpans"][0]["spans"].as_array().cloned().unwrap_or_default())
            .collect();
        if spans.len() >= 3 || std::time::Instant::now() > deadline {
            break spans;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    let request = spans.iter().find(|s| s["kind"] == 2).expect("the request's span");
    let mut attempts: Vec<&Value> = spans.iter().filter(|s| s["kind"] == 3).collect();
    attempts.sort_by_key(|s| attr(s, "gw.attempt").as_str().map(str::to_owned));
    assert_eq!(attempts.len(), 2, "{spans:?}");
    for a in &attempts {
        assert_eq!(a["parentSpanId"], request["spanId"]);
        assert_eq!(a["traceId"], request["traceId"]);
    }
    assert_eq!(attr(attempts[0], "http.response.status_code"), json!("503"));
    assert_eq!(attempts[0]["status"]["code"], 2, "the failed attempt is an error");
    assert_eq!(attr(attempts[1], "http.response.status_code"), json!("200"));
    assert!(attr(attempts[1], "gw.upstream_request_id").as_str().is_some_and(|id| !id.is_empty()), "{spans:?}");
}
