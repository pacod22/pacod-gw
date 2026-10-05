//! Proxy mode end to end: a gateway in process, a scripted mock provider, a real PostgreSQL.

mod common;

use std::time::Duration;

use common::{IngestSink, PROVIDER_SECRET, TestGw};
use futures_util::StreamExt;
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

/// A body with spacing and escapes a re-serialiser would change.
const CHAT_BODY: &str = "{ \"model\" : \"gpt-test\",\n  \"messages\":[{\"role\":\"user\",\"content\":\"Gr\\u00fc\\u00dfe  aus  Sofia\"}] ,\"max_tokens\": 64 }";

async fn setup(family: &str) -> (TestGw, MockProvider, IngestSink, common::Fixture) {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let base = if family == "openai" { format!("{}/v1", mock.base_url()) } else { mock.base_url() };
    let fx = gw.fixture(family, &base).await;
    (gw, mock, sink, fx)
}

#[tokio::test]
async fn request_and_response_bodies_are_relayed_byte_for_byte() {
    let (gw, mock, sink, fx) = setup("openai").await;

    let resp = gw
        .http
        .post(format!("{}/proxy/openai/v1/chat/completions?trace=1", gw.data))
        .bearer_auth(&fx.key)
        .header("content-type", "application/json")
        .header("x-gw-end-user", "u_18342")
        .header("x-gw-trace", "turn_9f3")
        .header("x-gw-tags", "purpose=planner,site=pacod,undeclared=x")
        .body(CHAT_BODY)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let request_id = resp.headers()["x-gw-request-id"].to_str().unwrap().to_owned();
    assert!(request_id.starts_with("gw_"));
    assert_eq!(resp.headers()["x-gw-attempts"], "1");
    let delay: f32 = resp.headers()["x-gw-delay-ms"].to_str().unwrap().parse().unwrap();
    assert!((0.0..250.0).contains(&delay), "gateway delay header: {delay}");
    let got = resp.bytes().await.unwrap();

    // What the provider received.
    let captured = mock.captured();
    assert_eq!(captured.len(), 1);
    let c = &captured[0];
    assert_eq!(c.method, "POST");
    assert_eq!(c.path, "/v1/chat/completions?trace=1");
    assert_eq!(&c.body[..], CHAT_BODY.as_bytes(), "the request body must reach the provider unchanged");
    assert_eq!(c.header("authorization"), Some(format!("Bearer {PROVIDER_SECRET}").as_str()));
    assert!(c.headers.iter().all(|(n, _)| !n.starts_with("x-gw-")), "gateway headers are not forwarded");
    assert!(c.headers.iter().all(|(_, v)| !v.contains(&fx.key)), "the gateway key never reaches the provider");
    assert_eq!(c.header("accept-encoding"), Some("identity"));

    // What the caller received: the same bytes the provider answers a direct call with.
    let direct = gw
        .http
        .post(format!("{}/v1/chat/completions", mock.base_url()))
        .bearer_auth(PROVIDER_SECRET)
        .header("content-type", "application/json")
        .body(CHAT_BODY)
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let strip_ids = |b: &[u8]| -> Value {
        let mut v: Value = serde_json::from_slice(b).unwrap();
        v.as_object_mut().unwrap().remove("id");
        v.as_object_mut().unwrap().remove("created");
        v
    };
    assert_eq!(strip_ids(&got), strip_ids(&direct));
    let body: Value = serde_json::from_slice(&got).unwrap();

    // The usage event.
    let ev = sink.wait_events(1).await;
    let e = &ev[0];
    assert_eq!(format!("gw_{}", e.id.simple()), request_id);
    assert_eq!((e.mode.as_str(), e.interface.as_str(), e.provider.as_str()), ("proxy", "proxy", "openai"));
    assert_eq!(
        (e.organisation_id.as_str(), e.workspace_id.as_str(), e.key_id.as_str()),
        (fx.org.as_str(), fx.ws.as_str(), fx.key_id.as_str())
    );
    assert_eq!(e.model_requested, "gpt-test");
    assert_eq!((e.status.as_str(), e.http_status, e.attempts), ("ok", 200, 1));
    assert_eq!(e.usage_source, "provider");
    assert_eq!(e.usage.prompt_total(), body["usage"]["prompt_tokens"].as_u64().unwrap());
    assert_eq!(e.usage.output_tokens, body["usage"]["completion_tokens"].as_u64().unwrap());
    assert_eq!(e.finish_reason, "stop");
    assert_eq!(e.end_user, "u_18342");
    assert_eq!(e.trace_id, "turn_9f3");
    assert_eq!(e.tags.get("purpose").map(String::as_str), Some("planner"));
    assert_eq!(e.tags.get("site").map(String::as_str), Some("pacod"));
    assert!(!e.tags.contains_key("undeclared"), "undeclared tag keys are dropped");
    assert_eq!(e.path, "/v1/chat/completions");
    assert!(!e.upstream_request_id.is_empty());
    // Timing: every stage is present and consistent.
    let t = &e.timing;
    assert!(t.total_ms > 0.0 && t.pre_ms >= 0.0 && t.post_ms >= 0.0);
    assert!((t.gateway_delay_ms - (t.pre_ms + t.post_ms)).abs() < 0.01);
    assert!(t.gateway_delay_ms < t.total_ms);
    assert!(t.upstream_first_byte_ms > 0.0);

    // The batch was authenticated with the internal token.
    let b = &sink.batches()[0];
    assert_eq!(b.token, "test-internal-token-0123456789");
    assert_eq!(b.batch_id.split('-').count(), 4, "spool id, segment, start and end: {}", b.batch_id);
}

#[tokio::test]
async fn streams_are_relayed_unchanged_and_usage_is_read_from_them() {
    let (gw, mock, sink, fx) = setup("openai").await;
    let body = json!({
        "model": "gpt-test", "stream": true, "stream_options": { "include_usage": true },
        "messages": [{ "role": "user", "content": "one two three four five six" }]
    })
    .to_string();

    let send = |url: String, key: String, mock_opts: &'static str| {
        let (http, body) = (gw.http.clone(), body.clone());
        async move {
            let r = http
                .post(url)
                .bearer_auth(key)
                .header("content-type", "application/json")
                .header("x-mock", mock_opts)
                .body(body)
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            assert!(r.headers()["content-type"].to_str().unwrap().starts_with("text/event-stream"));
            r.bytes().await.unwrap()
        }
    };
    // `split=7` makes events span chunk boundaries on the way through the gateway.
    let via_gateway =
        send(format!("{}/proxy/openai/v1/chat/completions", gw.data), fx.key.clone(), "split=7,request_id=req_fixed")
            .await;
    let direct =
        send(format!("{}/v1/chat/completions", mock.base_url()), PROVIDER_SECRET.to_owned(), "request_id=req_fixed")
            .await;

    let events = |b: &[u8]| -> Vec<Value> {
        String::from_utf8_lossy(b)
            .split("\n\n")
            .filter_map(|e| e.strip_prefix("data: "))
            .filter(|d| *d != "[DONE]")
            .map(|d| {
                let mut v: Value = serde_json::from_str(d).unwrap();
                v.as_object_mut().unwrap().remove("id");
                v.as_object_mut().unwrap().remove("created");
                v
            })
            .collect()
    };
    assert_eq!(events(&via_gateway), events(&direct));
    assert!(via_gateway.ends_with(b"data: [DONE]\n\n"));

    let e = &sink.wait_events(1).await[0];
    assert!(e.stream);
    assert_eq!((e.status.as_str(), e.usage_source.as_str()), ("ok", "provider"));
    let last = events(&via_gateway).into_iter().rev().find(|v| !v["usage"].is_null()).unwrap();
    assert_eq!(e.usage.prompt_total(), last["usage"]["prompt_tokens"].as_u64().unwrap());
    assert_eq!(e.usage.output_tokens, last["usage"]["completion_tokens"].as_u64().unwrap());
    assert_eq!(e.upstream_request_id, "req_fixed");
    assert!(e.timing.upstream_stream_ms >= 0.0);
}

#[tokio::test]
async fn anthropic_sdk_style_call_with_cache_usage() {
    let (gw, mock, sink, fx) = setup("anthropic").await;
    let body = r#"{"model":"claude-test","max_tokens":128,"stream":true,"system":[{"type":"text","text":"You are careful.","cache_control":{"type":"ephemeral"}}],"messages":[{"role":"user","content":"Hello there"}]}"#;
    let r = gw
        .http
        .post(format!("{}/proxy/anthropic/v1/messages", gw.data))
        // The Anthropic SDK sends the key in x-api-key.
        .header("x-api-key", &fx.key)
        .header("anthropic-version", "2023-06-01")
        .header("anthropic-beta", "some-beta-2026-01-01")
        .header("content-type", "application/json")
        .header("x-mock", "cache_read=6100,cache_write=300,thinking")
        .body(body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    // Rate-limit headers describe the shared provider key and are not passed on.
    assert!(r.headers().keys().all(|k| !k.as_str().contains("ratelimit")), "{:?}", r.headers());
    let text = r.text().await.unwrap();
    assert!(text.contains("event: message_start") && text.contains("event: message_stop"));
    assert!(text.contains("thinking_delta") && text.contains("signature_delta"));

    let c = &mock.captured()[0];
    assert_eq!(&c.body[..], body.as_bytes());
    assert_eq!(c.header("x-api-key"), Some(PROVIDER_SECRET));
    assert_eq!(c.header("anthropic-version"), Some("2023-06-01"));
    assert_eq!(c.header("anthropic-beta"), Some("some-beta-2026-01-01"));
    assert!(c.header("authorization").is_none());

    let e = &sink.wait_events(1).await[0];
    assert_eq!(e.provider, "anthropic");
    assert_eq!(e.usage.cache_read_tokens, 6100);
    assert_eq!(e.usage.cache_write_tokens, 300);
    assert!(e.usage.input_tokens > 0 && e.usage.output_tokens > 0);
    assert_eq!(e.finish_reason, "end_turn");
    assert!(e.stream);
}

#[tokio::test]
async fn gateway_errors_use_the_callers_dialect() {
    let (gw, mock, _sink, fx) = setup("anthropic").await;
    let url = format!("{}/proxy/anthropic/v1/messages", gw.data);

    // No key and a wrong key: 401 in Anthropic's envelope, nothing reaches the provider.
    for key in [None, Some("pgw_0000_wrong"), Some("sk-ant-not-a-gateway-key")] {
        let mut req = gw.http.post(&url).header("content-type", "application/json").body("{}");
        if let Some(k) = key {
            req = req.header("x-api-key", k);
        }
        let r = req.send().await.unwrap();
        assert_eq!(r.status(), 401);
        assert_eq!(r.headers()["x-gw-error-type"], "invalid_key");
        assert_eq!(r.headers()["x-should-retry"], "false");
        let b: Value = r.json().await.unwrap();
        assert_eq!((b["type"].as_str(), b["error"]["type"].as_str()), (Some("error"), Some("authentication_error")));
    }
    assert!(mock.captured().is_empty());

    // A path that is not relayed for this provider.
    let r = gw
        .http
        .post(format!("{}/proxy/anthropic/admin/keys", gw.data))
        .header("x-api-key", &fx.key)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    assert_eq!(r.json::<Value>().await.unwrap()["error"]["type"], "permission_error");

    // An unknown provider, after authentication.
    let r = gw.http.post(format!("{}/proxy/nope/v1/x", gw.data)).bearer_auth(&fx.key).send().await.unwrap();
    assert_eq!(r.status(), 404);
    assert_eq!(r.json::<Value>().await.unwrap()["error"]["type"], "provider_not_found");

    // A body that repeats a key the gateway reads.
    let r = gw
        .http
        .post(&url)
        .header("x-api-key", &fx.key)
        .header("content-type", "application/json")
        .body(r#"{"model":"a","model":"b","max_tokens":1,"messages":[]}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 400);
    let b: Value = r.json().await.unwrap();
    assert_eq!(b["error"]["type"], "invalid_request_error");
    assert!(b["error"]["message"].as_str().unwrap().contains("more than once"));
    assert!(mock.captured().is_empty(), "rejected requests never reach the provider");
}

#[tokio::test]
async fn usage_policy_decides_what_happens_without_provider_usage() {
    let (gw, mock, sink, fx) = setup("openai").await;
    let url = format!("{}/proxy/openai/v1/chat/completions", gw.data);
    let no_usage_stream =
        json!({ "model": "gpt-test", "stream": true, "messages": [{ "role": "user", "content": "hi there" }] });

    // Default policy `estimate`: forwarded, and the event carries an estimate.
    let r = gw.http.post(&url).bearer_auth(&fx.key).json(&no_usage_stream).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let text = r.text().await.unwrap();
    assert!(!text.contains("\"prompt_tokens\""), "nothing is injected: the stream has no usage chunk");
    let e = sink.wait_events(1).await.remove(0);
    assert_eq!(e.usage_source, "estimated");
    assert!(e.usage.input_tokens > 0 && e.usage.output_tokens > 0);
    assert_eq!(mock.captured()[0].json().unwrap().get("stream_options"), None);

    // Policy `require`: rejected before forwarding, in OpenAI's envelope.
    let key_path = format!("/orgs/{}/workspaces/{}/keys/{}", fx.org, fx.ws, fx.key_id);
    let (st, _) = gw.api("PUT", &key_path, Some(json!({ "name": "svc", "usage_policy": "require" }))).await;
    assert_eq!(st, 200);
    mock.clear();
    let r = gw.http.post(&url).bearer_auth(&fx.key).json(&no_usage_stream).send().await.unwrap();
    assert_eq!(r.status(), 400);
    let b: Value = r.json().await.unwrap();
    assert_eq!(b["error"]["code"], "usage_required");
    assert!(mock.captured().is_empty());
    // A non-streamed call always reports usage and passes.
    let r = gw
        .http
        .post(&url)
        .bearer_auth(&fx.key)
        .json(&json!({ "model": "gpt-test", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    // Policy `ignore`: forwarded, tokens stay empty.
    let (st, _) = gw.api("PUT", &key_path, Some(json!({ "name": "svc", "usage_policy": "ignore" }))).await;
    assert_eq!(st, 200);
    let r = gw.http.post(&url).bearer_auth(&fx.key).json(&no_usage_stream).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let _ = r.bytes().await;
    let ev = sink.wait_events(4).await;
    let last = ev.iter().find(|e| e.usage_source == "missing" && e.status == "ok").expect("an event without usage");
    assert!(last.usage.is_empty());
    // The rejected call is recorded too, as rejected.
    assert!(ev.iter().any(|e| e.status == "rejected" && e.error_type == "usage_required"));
}

#[tokio::test]
async fn a_timeout_is_never_retried_and_a_connect_failure_is_retried_once() {
    let (gw, mock, sink, fx) = setup("openai").await;
    // One second for the provider to answer.
    let (st, _) = gw
        .api(
            "PUT",
            &format!("/providers/{}", fx.provider),
            Some(json!({ "name": "openai", "family": "openai", "base_url": format!("{}/v1", mock.base_url()), "default_timeout_seconds": 1 })),
        )
        .await;
    assert_eq!(st, 200);

    let r = gw
        .http
        .post(format!("{}/proxy/openai/v1/chat/completions", gw.data))
        .bearer_auth(&fx.key)
        .header("x-mock", "hang")
        .json(&json!({ "model": "gpt-test", "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 504);
    assert_eq!(r.headers()["x-gw-error-type"], "upstream_timeout");
    assert_eq!(r.headers()["x-should-retry"], "false");
    assert_eq!(mock.captured().len(), 1, "the provider saw exactly one request");
    let e = sink.wait_events(1).await.remove(0);
    assert_eq!((e.status.as_str(), e.error_type.as_str(), e.attempts), ("timeout", "upstream_timeout", 1));

    // A provider nobody listens on: nothing can have been sent, so a second attempt is made.
    let dead = {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        l.local_addr().unwrap()
    };
    let p = gw
        .create("/providers", json!({ "name": "dead", "family": "openai", "base_url": format!("http://{dead}/v1") }))
        .await
        .id;
    gw.create(&format!("/providers/{p}/credentials"), json!({ "secret": "x" })).await;
    let dead_model = gw.draft_model(&p, "dead/m", "m", json!({ "free": true })).await;
    gw.activate(&dead_model).await;
    gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": dead_model })).await;
    let r = gw
        .http
        .post(format!("{}/proxy/dead/v1/chat/completions", gw.data))
        .bearer_auth(&fx.key)
        .json(&json!({ "model": "m", "messages": [] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 502);
    assert_eq!(r.json::<Value>().await.unwrap()["error"]["code"], "upstream_error");
    let ev = sink.wait_events(2).await;
    let e = ev.iter().find(|e| e.provider == "dead").unwrap();
    assert_eq!((e.status.as_str(), e.attempts), ("error", 2));
    assert!(e.timing.retry_ms >= 0.0);
}

#[tokio::test]
async fn broken_and_cancelled_streams_are_recorded() {
    let (gw, mock, sink, fx) = setup("openai").await;
    let url = format!("{}/proxy/openai/v1/chat/completions", gw.data);
    let body = json!({
        "model": "gpt-test", "stream": true, "stream_options": { "include_usage": true },
        "messages": [{ "role": "user", "content": "a b c d e f g h i j k l" }]
    });

    // The provider aborts the connection in the middle of the stream.
    let r = gw.http.post(&url).bearer_auth(&fx.key).header("x-mock", "reset_after=2").json(&body).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.bytes().await.is_err(), "the client sees the stream break");
    let e = sink.wait_events(1).await.remove(0);
    assert_eq!((e.status.as_str(), e.error_type.as_str()), ("error", "stream"));

    // The client goes away in the middle of the stream.
    let r = gw
        .http
        .post(&url)
        .bearer_auth(&fx.key)
        .header("x-mock", "chunk_delay=150,chunks=12")
        .json(&body)
        .send()
        .await
        .unwrap();
    let mut stream = r.bytes_stream();
    let first = stream.next().await.unwrap().unwrap();
    assert!(!first.is_empty());
    drop(stream);
    let ev = sink.wait_events(2).await;
    let e = ev.iter().find(|e| e.status == "cancelled").expect("a cancelled event");
    assert_eq!(e.error_type, "cancelled");
    assert!(e.timing.total_ms < 1500.0, "settled when the client left, not when the stream would have ended");
    let _ = mock;
}

#[tokio::test]
async fn blind_relay_streams_the_body_without_reading_it() {
    let (gw, mock, sink, fx) = setup("openai").await;
    let key = gw
        .create(
            &format!("/orgs/{}/workspaces/{}/keys", fx.org, fx.ws),
            json!({ "name": "blind", "inspect": "none", "usage_policy": "ignore", "allowed_modes": ["proxy"] }),
        )
        .await;
    let secret = key.body["secret"].as_str().unwrap();
    // Not even valid JSON: a blind relay does not look.
    let r = gw
        .http
        .post(format!("{}/proxy/openai/v1/embeddings", gw.data))
        .bearer_auth(secret)
        .header("content-type", "application/json")
        .body("{\"model\":\"m\",\"model\":\"dup\",\"input\":\"x\"}")
        .send()
        .await
        .unwrap();
    let _ = r.status();
    assert_eq!(&mock.captured()[0].body[..], b"{\"model\":\"m\",\"model\":\"dup\",\"input\":\"x\"}");
    let e = sink.wait_events(1).await.remove(0);
    assert_eq!(e.model_requested, "", "a blind relay does not know the model");
    assert_eq!(e.key_id, key.id);

    // Blind relay cannot be combined with accounting requirements.
    let (st, v) = gw
        .api(
            "POST",
            &format!("/orgs/{}/workspaces/{}/keys", fx.org, fx.ws),
            Some(json!({ "name": "bad", "inspect": "none", "usage_policy": "require" })),
        )
        .await;
    assert_eq!(st, 422, "{v}");
}

#[tokio::test]
async fn revocation_and_rotation_take_effect_at_once() {
    let (gw, _mock, _sink, fx) = setup("openai").await;
    let url = format!("{}/proxy/openai/v1/chat/completions", gw.data);
    let call = |key: String| {
        let (http, url) = (gw.http.clone(), url.clone());
        async move {
            http.post(url)
                .bearer_auth(key)
                .json(&json!({ "model": "m", "messages": [] }))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    assert_eq!(call(fx.key.clone()).await, 200);

    // Rotation: the new secret works, the old one keeps working during the grace period.
    let key_path = format!("/orgs/{}/workspaces/{}/keys/{}", fx.org, fx.ws, fx.key_id);
    let (st, rotated) = gw.api("POST", &format!("{key_path}/rotate"), Some(json!({ "grace_hours": 1 }))).await;
    assert_eq!(st, 200);
    let new_key = rotated["secret"].as_str().unwrap().to_owned();
    assert_ne!(new_key, fx.key);
    assert_eq!(call(new_key.clone()).await, 200);
    assert_eq!(call(fx.key.clone()).await, 200, "the previous secret is valid during the grace period");

    // Rotation without grace: the old secret stops at once.
    let (_, rotated) = gw.api("POST", &format!("{key_path}/rotate"), Some(json!({ "grace_hours": 0 }))).await;
    let newest = rotated["secret"].as_str().unwrap().to_owned();
    assert_eq!(call(new_key).await, 401);
    assert_eq!(call(newest.clone()).await, 200);

    // Revocation.
    let (st, _) = gw.api("POST", &format!("{key_path}/revoke"), None).await;
    assert_eq!(st, 200);
    assert_eq!(call(newest).await, 401);
}

#[tokio::test]
async fn credential_rotation_is_used_by_the_next_request() {
    let (gw, mock, _sink, fx) = setup("openai").await;
    let url = format!("{}/proxy/openai/v1/chat/completions", gw.data);
    let body = json!({ "model": "m", "messages": [] });
    gw.http.post(&url).bearer_auth(&fx.key).json(&body).send().await.unwrap();
    assert_eq!(mock.captured()[0].header("authorization"), Some(format!("Bearer {PROVIDER_SECRET}").as_str()));

    gw.create(
        &format!("/providers/{}/credentials", fx.provider),
        json!({ "label": "rotated", "secret": "sk-rotated-NEW1" }),
    )
    .await;
    mock.clear();
    gw.http.post(&url).bearer_auth(&fx.key).json(&body).send().await.unwrap();
    assert_eq!(mock.captured()[0].header("authorization"), Some("Bearer sk-rotated-NEW1"));

    // The replaced credential is kept as retiring, and no listing reveals a secret.
    let (_, list) = gw.api("GET", &format!("/providers/{}/credentials", fx.provider), None).await;
    let states: Vec<&str> = list["items"].as_array().unwrap().iter().map(|c| c["state"].as_str().unwrap()).collect();
    assert_eq!(states, ["retiring", "current"]);
    let text = list.to_string();
    assert!(!text.contains(PROVIDER_SECRET) && !text.contains("sk-rotated-NEW1"));
    assert!(text.contains("NEW1"), "the fingerprint shows the last four characters");

    // A workspace's own credential takes precedence over the shared one.
    gw.create(
        &format!("/providers/{}/credentials", fx.provider),
        json!({ "secret": "sk-own-WS01", "scope": { "type": "workspace", "id": fx.ws } }),
    )
    .await;
    mock.clear();
    let r = gw.http.post(&url).bearer_auth(&fx.key).json(&body).send().await.unwrap();
    assert_eq!(mock.captured()[0].header("authorization"), Some("Bearer sk-own-WS01"));
    // With its own credential the workspace is shown the provider's rate-limit headers.
    let has_rl = r.headers().keys().any(|k| k.as_str().contains("ratelimit"));
    assert!(has_rl, "{:?}", r.headers());
    tokio::time::sleep(Duration::from_millis(10)).await;
}

#[tokio::test]
async fn a_path_without_usage_is_counted_as_a_request_only() {
    let (gw, mock, sink, fx) = setup("openai").await;
    // The provider's model list: no model in the request, nothing to meter.
    let r = gw.http.get(format!("{}/proxy/openai/v1/models", gw.data)).bearer_auth(&fx.key).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers().get("x-gw-model-served").is_none());
    let listed = r.json::<Value>().await.unwrap();
    assert_eq!(listed["object"], "list");
    assert_eq!(mock.captured()[0].method, "GET");

    let e = sink.wait_events(1).await.remove(0);
    assert_eq!((e.status.as_str(), e.http_status, e.path.as_str()), ("ok", 200, "/v1/models"));
    assert_eq!(e.usage_source, "missing", "bytes of a model list are not tokens");
    assert!(e.usage.is_empty() && e.cost.is_none() && !e.price_missing);
    assert_eq!(e.model_served, "");
}
