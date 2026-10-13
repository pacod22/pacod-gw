//! The minimal gateway end to end, in process, against the mock providers. No database.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pacod_gw_core::relay::CallSink;
use pacod_gw_events::{AuditRecord, UsageEvent};
use pacod_gw_lite::config;
use pacod_gw_lite::env::Vars;
use pacod_gw_lite::server::Server;
use pacod_gw_testkit::mock::{Behavior, MockProvider};
use serde_json::{Value, json};

const OPENAI_KEY: &str = "sk-openai-test-secret";
const ANTHROPIC_KEY: &str = "sk-ant-test-secret";
const APP: &str = "pgw_app_AppSecret0123456789abcdef";
const LIMITED: &str = "pgw_limited_LimitedSecret0123456789";
const SLOW: &str = "pgw_slow_SlowSecret0123456789abcd";
const METERED: &str = "pgw_metered_MeteredSecret012345678";

/// Every settled call, as the log would get it.
#[derive(Default)]
struct Capture(Mutex<Vec<(UsageEvent, Option<AuditRecord>)>>);

impl CallSink for Capture {
    fn settled(&self, e: &UsageEvent, audit: Option<&AuditRecord>) {
        self.0.lock().unwrap().push((e.clone(), audit.cloned()));
    }
}

struct Lite {
    server: Server,
    url: String,
    mock: MockProvider,
    calls: Arc<Capture>,
    http: reqwest::Client,
}

fn vars(mock: &MockProvider, extra: &[(&str, &str)]) -> Vars {
    let openai = format!("{}/v1", mock.base_url());
    let mut pairs: Vec<(String, String)> = [
        ("PGW_LISTEN", "127.0.0.1:0"),
        ("PGW_EGRESS_ALLOW_PRIVATE", "127.0.0.0/8"),
        ("PGW_DRAIN_SECONDS", "1"),
        ("PGW_PROVIDER_OPENAI_KEY", OPENAI_KEY),
        ("PGW_PROVIDER_OPENAI_BASE_URL", &openai),
        ("PGW_PROVIDER_ANTHROPIC_KEY", ANTHROPIC_KEY),
        ("PGW_PROVIDER_ANTHROPIC_BASE_URL", &mock.base_url()),
        ("PGW_MODEL_GPT", "openai/gpt-test"),
        ("PGW_MODEL_GPT_PRICE", "input=1,output=2"),
        ("PGW_MODEL_CLAUDE", "anthropic/claude-test"),
        ("PGW_MODEL_CLAUDE_PRICE", "input=3,output=15"),
        ("PGW_MODEL_EMBED", "openai/text-embed-test"),
        ("PGW_MODEL_TTS", "openai/tts-test"),
        ("PGW_POOL_FAST", "openai/gpt-test,anthropic/claude-test"),
        ("PGW_CLIENT_APP_KEY", APP),
        ("PGW_CLIENT_LIMITED_KEY", LIMITED),
        ("PGW_CLIENT_LIMITED_RPM", "2"),
        ("PGW_CLIENT_LIMITED_MODELS", "openai/gpt-test"),
        ("PGW_CLIENT_SLOW_KEY", SLOW),
        ("PGW_CLIENT_SLOW_CONCURRENCY", "1"),
        ("PGW_CLIENT_METERED_KEY", METERED),
        ("PGW_CLIENT_METERED_TPM", "100000"),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_owned(), v.to_owned()))
    .collect();
    for (k, v) in extra {
        pairs.retain(|(name, _)| name != k);
        pairs.push(((*k).to_owned(), (*v).to_owned()));
    }
    Vars::from_pairs(pairs)
}

async fn start_with(extra: &[(&str, &str)], sink: Option<Arc<dyn CallSink>>) -> Lite {
    let mock = MockProvider::spawn().await;
    let cfg = config::read(&vars(&mock, extra)).expect("the test configuration reads");
    let calls = Arc::new(Capture::default());
    let server = Server::start(&cfg, sink.unwrap_or_else(|| calls.clone())).await.expect("the gateway starts");
    let url = format!("http://{}", server.addr);
    Lite { server, url, mock, calls, http: reqwest::Client::new() }
}

async fn start(extra: &[(&str, &str)]) -> Lite {
    start_with(extra, None).await
}

impl Lite {
    async fn openai(&self, key: &str, body: Value) -> (u16, Value) {
        let r = self
            .http
            .post(format!("{}/v1/chat/completions", self.url))
            .bearer_auth(key)
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    async fn anthropic(&self, key: &str, body: Value) -> (u16, Value) {
        let r = self
            .http
            .post(format!("{}/v1/messages", self.url))
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(Value::Null))
    }

    /// The calls settled so far, once `n` of them are.
    async fn calls(&self, n: usize) -> Vec<(UsageEvent, Option<AuditRecord>)> {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let calls = self.calls.0.lock().unwrap().clone();
            if calls.len() >= n || Instant::now() > deadline {
                return calls;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    async fn last_call(&self, n: usize) -> UsageEvent {
        let calls = self.calls(n).await;
        assert_eq!(calls.len(), n, "settled calls: {:?}", calls.iter().map(|(e, _)| &e.status).collect::<Vec<_>>());
        calls[n - 1].0.clone()
    }
}

fn chat(model: &str, text: &str) -> Value {
    json!({ "model": model, "messages": [{ "role": "user", "content": text }] })
}

fn message(model: &str, text: &str) -> Value {
    json!({ "model": model, "max_tokens": 64, "messages": [{ "role": "user", "content": text }] })
}

#[tokio::test]
async fn each_client_api_reaches_its_provider_with_the_providers_key() {
    let lite = start(&[]).await;

    let (status, body) = lite.openai(APP, chat("openai/gpt-test", "hello world")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["choices"][0]["message"]["content"], "echo: hello world");
    let sent = &lite.mock.captured()[0];
    assert_eq!(sent.path, "/v1/chat/completions");
    assert_eq!(sent.header("authorization"), Some(format!("Bearer {OPENAI_KEY}").as_str()));
    assert_eq!(sent.json().unwrap()["model"], "gpt-test", "the provider gets its own model id");

    let (status, body) = lite.anthropic(APP, message("anthropic/claude-test", "hello there")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["content"][0]["text"], "echo: hello there");
    let sent = &lite.mock.captured()[1];
    assert_eq!((sent.path.as_str(), sent.header("x-api-key")), ("/v1/messages", Some(ANTHROPIC_KEY)));

    let e = lite.last_call(2).await;
    assert_eq!((e.key_id.as_str(), e.status.as_str(), e.http_status), ("app", "ok", 200));
    assert_eq!((e.provider.as_str(), e.model_served.as_str()), ("anthropic", "anthropic/claude-test"));
    // Input 3 tokens ("hello there"), output 5 ("echo: hello there"), at 3 and 15 per million.
    assert_eq!((e.usage.input_tokens, e.usage.output_tokens), (3, 5));
    assert_eq!(e.currency, "USD");
    assert!((e.cost.unwrap() - (3.0 * 3.0 + 5.0 * 15.0) / 1e6).abs() < 1e-12, "{:?}", e.cost);
    lite.server.shutdown().await;
}

#[tokio::test]
async fn calls_are_translated_between_dialects_and_streamed() {
    let lite = start(&[]).await;

    // OpenAI's API, an Anthropic model.
    let (status, body) = lite.openai(APP, chat("anthropic/claude-test", "translate me")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["choices"][0]["message"]["content"], "echo: translate me");
    assert_eq!(lite.mock.captured()[0].path, "/v1/messages");

    // Anthropic's API, an OpenAI model.
    let (status, body) = lite.anthropic(APP, message("openai/gpt-test", "and back")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["content"][0]["text"], "echo: and back");
    assert_eq!(lite.mock.captured()[1].path, "/v1/chat/completions");

    // A stream, with usage reported at its end.
    let mut req = chat("openai/gpt-test", "stream this please");
    req["stream"] = json!(true);
    req["stream_options"] = json!({ "include_usage": true });
    let r =
        lite.http.post(format!("{}/v1/chat/completions", lite.url)).bearer_auth(APP).json(&req).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let text = r.text().await.unwrap();
    assert!(text.contains("data: [DONE]"), "{text}");
    let pieces: String = text
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|d| serde_json::from_str::<Value>(d).ok())
        .filter_map(|v| v["choices"][0]["delta"]["content"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(pieces, "echo: stream this please");

    let e = lite.last_call(3).await;
    assert!(e.stream);
    assert_eq!((e.status.as_str(), e.usage_source.as_str()), ("ok", "provider"));
    lite.server.shutdown().await;
}

#[tokio::test]
async fn proxy_mode_relays_with_the_operators_key_and_estimates_missing_usage() {
    let lite = start(&[]).await;

    // A stream that does not ask for usage, from a client with a tokens-per-minute limit: the
    // gateway estimates instead of refusing it.
    let r = lite
        .http
        .post(format!("{}/proxy/openai/v1/chat/completions", lite.url))
        .bearer_auth(METERED)
        .json(&json!({ "model": "gpt-test", "stream": true, "messages": [{ "role": "user", "content": "relay" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let text = r.text().await.unwrap();
    assert!(text.contains("relay") && text.contains("data: [DONE]"), "{text}");
    let sent = &lite.mock.captured()[0];
    assert_eq!(sent.path, "/v1/chat/completions");
    assert_eq!(sent.header("authorization"), Some(format!("Bearer {OPENAI_KEY}").as_str()));

    let e = lite.last_call(1).await;
    assert_eq!((e.mode.as_str(), e.key_id.as_str(), e.status.as_str()), ("proxy", "metered", "ok"));
    assert_eq!(e.model_served, "openai/gpt-test");
    assert_ne!(e.usage_source, "missing");
    lite.server.shutdown().await;
}

#[tokio::test]
async fn a_pool_fails_over_to_its_next_model() {
    let lite = start(&[]).await;
    lite.mock.set_script(Behavior::parse_all(["fail=503x1"]).unwrap());

    let (status, body) = lite.openai(APP, chat("@fast", "fail over")).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["choices"][0]["message"]["content"], "echo: fail over");
    let paths: Vec<String> = lite.mock.captured().iter().map(|r| r.path.clone()).collect();
    assert_eq!(paths, ["/v1/chat/completions", "/v1/messages"], "first model, then the next");

    let e = lite.last_call(1).await;
    assert_eq!((e.template.as_str(), e.model_served.as_str()), ("fast", "anthropic/claude-test"));
    assert_eq!((e.failovers, e.routing_scheme.as_str()), (1, "pool-fast"));
    lite.server.shutdown().await;
}

#[tokio::test]
async fn limits_and_allow_lists_refuse_calls() {
    let lite = start(&[]).await;

    // Two requests a minute.
    for _ in 0..2 {
        assert_eq!(lite.openai(LIMITED, chat("openai/gpt-test", "x")).await.0, 200);
    }
    let r = lite
        .http
        .post(format!("{}/v1/chat/completions", lite.url))
        .bearer_auth(LIMITED)
        .json(&chat("openai/gpt-test", "x"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 429);
    assert!(r.headers().get("x-gw-request-id").is_some_and(|v| !v.is_empty()), "a refused call is logged as a call");
    assert!(r.headers().contains_key("retry-after"));
    let e = lite.last_call(3).await;
    assert_eq!((e.status.as_str(), e.http_status, e.key_id.as_str()), ("rejected", 429, "limited"));

    // A model the client may not use.
    let (status, body) = lite.openai(LIMITED, chat("anthropic/claude-test", "x")).await;
    assert_eq!(status, 403, "{body}");
    let (status, body) = lite.openai(LIMITED, chat("@fast", "x")).await;
    assert!(status == 403 || status == 404, "no pool is allowed: {status} {body}");

    // It lists only what it may use: no pool, no other model.
    let r = lite.http.get(format!("{}/v1/models", lite.url)).bearer_auth(LIMITED).send().await.unwrap();
    let listed: Value = r.json().await.unwrap();
    let ids: Vec<&str> = listed["data"].as_array().unwrap().iter().filter_map(|m| m["id"].as_str()).collect();
    assert_eq!(ids, ["openai/gpt-test"]);

    // One call at a time: the second, while the first waits for the provider, is refused.
    lite.mock.set_script(Behavior::parse_all(["delay=1500"]).unwrap());
    let first = {
        let (http, url) = (lite.http.clone(), lite.url.clone());
        tokio::spawn(async move {
            http.post(format!("{url}/v1/chat/completions"))
                .bearer_auth(SLOW)
                .json(&chat("openai/gpt-test", "slow"))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(lite.openai(SLOW, chat("openai/gpt-test", "too many")).await.0, 429);
    assert_eq!(first.await.unwrap(), 200);
    lite.server.shutdown().await;
}

#[tokio::test]
async fn bad_keys_files_and_batches_and_the_rest_of_the_surface() {
    let lite = start(&[]).await;

    // A bad key never reaches the call log: no request id.
    let r = lite
        .http
        .post(format!("{}/v1/chat/completions", lite.url))
        .bearer_auth("pgw_app_WrongSecret0123456789abcdef")
        .json(&chat("openai/gpt-test", "x"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert!(r.headers().get("x-gw-request-id").is_none_or(|v| v.is_empty()), "no call was recorded");
    assert!(lite.mock.captured().is_empty());

    // Files and batches are the provider's own, through proxy mode.
    for path in ["/v1/files", "/v1/messages/batches"] {
        let r = lite.http.get(format!("{}{path}", lite.url)).header("x-api-key", APP).send().await.unwrap();
        assert_eq!(r.status(), 501, "{path}");
    }
    // A file the provider holds may be named: the credential is the operator's own.
    let mut body = message("anthropic/claude-test", "about the file");
    body["messages"][0]["content"] = json!([
        { "type": "document", "source": { "type": "file", "file_id": "file_abc" } },
        { "type": "text", "text": "about the file" }
    ]);
    let (status, answer) = lite.anthropic(APP, body).await;
    assert_eq!(status, 200, "{answer}");

    // Model lists, embeddings, speech, health and quota.
    let r = lite.http.get(format!("{}/v1/models", lite.url)).bearer_auth(APP).send().await.unwrap();
    let models: Value = r.json().await.unwrap();
    let ids: Vec<&str> = models["data"].as_array().unwrap().iter().filter_map(|m| m["id"].as_str()).collect();
    assert!(ids.contains(&"openai/gpt-test") && ids.contains(&"anthropic/claude-test"), "{ids:?}");
    let r = lite
        .http
        .post(format!("{}/v1/embeddings", lite.url))
        .bearer_auth(APP)
        .json(&json!({ "model": "openai/text-embed-test", "input": "vector please" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.json::<Value>().await.unwrap()["data"][0]["embedding"].as_array().map(Vec::len), Some(8));
    let r = lite
        .http
        .post(format!("{}/v1/audio/speech", lite.url))
        .bearer_auth(APP)
        .json(&json!({ "model": "openai/tts-test", "input": "say this", "voice": "alloy" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.headers()["content-type"].to_str().unwrap().starts_with("audio/"));
    assert_eq!(lite.http.get(format!("{}/readyz", lite.url)).send().await.unwrap().status(), 200);
    let started = Instant::now();
    let r = lite.http.get(format!("{}/gw/v1/quota", lite.url)).bearer_auth(LIMITED).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(started.elapsed() < Duration::from_millis(1500), "the quota endpoint does not wait");
    let quota: Value = r.json().await.unwrap();
    assert_eq!(quota["limits"][0]["amount"], 2.0);
    lite.server.shutdown().await;
}

#[tokio::test]
async fn bodies_are_logged_only_when_asked_for() {
    let long = "a long question ".repeat(40);

    let off = start(&[]).await;
    assert_eq!(off.openai(APP, chat("openai/gpt-test", &long)).await.0, 200);
    assert!(off.calls(1).await[0].1.is_none());
    off.server.shutdown().await;

    let on = start(&[("PGW_LOG_BODIES", "on"), ("PGW_LOG_BODY_MAX_BYTES", "256")]).await;
    assert_eq!(on.openai(APP, chat("openai/gpt-test", &long)).await.0, 200);
    let calls = on.calls(1).await;
    let (e, record) = &calls[0];
    let record = record.as_ref().expect("an audit record");
    assert_eq!(record.audit_id, e.audit_id);
    assert!(record.truncated);
    assert_eq!(record.client_request.body.len(), 256);
    assert!(record.client_request.body_bytes > 256);
    let auth = record.client_request.headers.iter().find(|(n, _)| n == "authorization").map(|(_, v)| v.as_str());
    assert_eq!(auth, Some("[gateway key app]"), "no credential is recorded");
    on.server.shutdown().await;

    let asked = start(&[("PGW_LOG_BODIES", "on_request")]).await;
    assert_eq!(asked.openai(APP, chat("openai/gpt-test", "not recorded")).await.0, 200);
    let r = asked
        .http
        .post(format!("{}/v1/chat/completions", asked.url))
        .bearer_auth(APP)
        .header("x-gw-audit", "true")
        .json(&chat("openai/gpt-test", "recorded"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let calls = asked.calls(2).await;
    assert!(calls[0].1.is_none());
    let response = &calls[1].1.as_ref().expect("asked for").client_response;
    assert!(response.body.contains("echo: recorded"), "{}", response.body);
    asked.server.shutdown().await;
}
