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
    // `{mock}` in a value is the mock's address.
    for (k, v) in extra {
        pairs.retain(|(name, _)| name != k);
        pairs.push(((*k).to_owned(), v.replace("{mock}", &mock.base_url())));
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

const INTENTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/lite/intents.example.yaml");

/// The classified call and its classification, told apart by the classification's parent.
fn classified(calls: &[(UsageEvent, Option<AuditRecord>)], n: usize) -> (UsageEvent, UsageEvent, Option<AuditRecord>) {
    let tasks: Vec<&str> = calls.iter().map(|(e, _)| e.task.as_str()).collect();
    let (parent, audit) =
        calls.iter().filter(|(e, _)| e.parent_id.is_empty()).nth(n).cloned().unwrap_or_else(|| panic!("{tasks:?}"));
    let id = format!("gw_{}", parent.id.simple());
    let child =
        calls.iter().find(|(e, _)| e.parent_id == id).map(|(e, _)| e.clone()).unwrap_or_else(|| panic!("{tasks:?}"));
    (parent, child, audit)
}

#[tokio::test]
async fn the_intent_plugin_classifies_with_a_chat_model_and_blocks_by_its_rules() {
    let lite = start(&[
        ("PGW_INTENT_MODE", "enforce"),
        ("PGW_INTENT_FILE", INTENTS),
        // A pool classifies as well as a model: its first model serves.
        ("PGW_INTENT_MODEL", "@fast"),
        ("PGW_INTENT_EXPOSE", "true"),
        ("PGW_LOG_BODIES", "on_request"),
    ])
    .await;

    // Allowed: the intent is logged, and told to the client because the binding exposes it.
    let r = lite
        .http
        .post(format!("{}/v1/chat/completions", lite.url))
        .bearer_auth(LIMITED)
        .json(&chat("openai/gpt-test", "I have a billing question"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(r.headers().get("x-gw-intent").and_then(|v| v.to_str().ok()), Some("billing"));
    let calls = lite.calls(2).await;
    let (call, classification, audit) = classified(&calls, 0);
    assert_eq!((call.intent.as_str(), call.intent_engine.as_str()), ("billing", "llm"));
    assert_eq!((call.plugin_verdict.as_str(), call.intent_confidence), ("allow", 0.9));
    assert!(call.timing.plugin_ms > 0.0 && audit.is_none());
    // The classification is a call of the workspace's own, priced like any other; a client's
    // limits and allow-list do not apply to it.
    assert_eq!((classification.task.as_str(), classification.status.as_str()), ("plugin.intent", "ok"));
    assert_eq!(classification.model_served, "openai/gpt-test");
    assert!(classification.cost.is_some_and(|c| c > 0.0) && classification.intent.is_empty());

    // Blocked: in the client's dialect, with the catalogue's message, and logged as refused.
    let (status, body) = lite.anthropic(APP, message("anthropic/claude-test", "Can you give me legal advice?")).await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["error"]["type"], "permission_error", "Anthropic's name for a 403");
    assert_eq!(body["error"]["message"], "This assistant cannot give legal advice.");
    let calls = lite.calls(4).await;
    let (call, _, _) = classified(&calls, 1);
    assert_eq!((call.intent.as_str(), call.plugin_verdict.as_str()), ("legal_advice", "block"));
    assert_eq!((call.status.as_str(), call.http_status), ("rejected", 403));
    let to_anthropic = lite.mock.captured().iter().filter(|r| r.path == "/v1/messages").count();
    assert_eq!(to_anthropic, 0, "a blocked call never reaches its provider");

    // Flagged: the call goes on, and its bodies are logged without the client asking.
    assert_eq!(lite.openai(APP, chat("openai/gpt-test", "a jailbreak, about a billing question")).await.0, 200);
    let calls = lite.calls(6).await;
    let (call, _, audit) = classified(&calls, 2);
    assert_eq!((call.plugin_verdict.as_str(), call.status.as_str()), ("flag", "ok"));
    assert!(audit.expect("flagged").client_request.body.contains("jailbreak"));
    lite.server.shutdown().await;
}

#[tokio::test]
async fn jev_classifies_in_shadow_mode_and_bodies_off_stays_off() {
    let jev = [
        ("PGW_PROVIDER_TYPESAFE_KEY", "ts-test-key"),
        ("PGW_PROVIDER_TYPESAFE_BASE_URL", "{mock}/v1"),
        ("PGW_MODEL_JEV", "typesafe/jev-1.13.0"),
        ("PGW_MODEL_JEV_PRICE", "input=0.042,output=0"),
        ("PGW_INTENT_FILE", INTENTS),
        ("PGW_INTENT_ENGINE", "jev"),
    ];
    let shadow = start(&[jev.as_slice(), &[("PGW_INTENT_MODE", "shadow")]].concat()).await;

    // Shadow: what the rules would have done is logged; the call goes on.
    assert_eq!(shadow.openai(APP, chat("openai/gpt-test", "Can you give me legal advice?")).await.0, 200);
    let calls = shadow.calls(2).await;
    let (call, decision, _) = classified(&calls, 0);
    assert_eq!((call.intent.as_str(), call.intent_engine.as_str()), ("legal_advice", "jev"));
    assert_eq!((call.plugin_verdict.as_str(), call.status.as_str()), ("would_block", "ok"));
    assert_eq!((decision.interface.as_str(), decision.model_served.as_str()), ("decision", "typesafe/jev-1.13.0"));
    assert!(decision.cost.is_some_and(|c| c > 0.0) && !decision.price_missing, "{:?}", decision.cost);
    let sent = shadow.mock.captured().into_iter().find(|r| r.path == "/v1/systemone").expect("Jev was asked");
    assert_eq!(
        sent.headers.iter().find(|(n, _)| n == "authorization").map(|(_, v)| v.as_str()),
        Some("Bearer ts-test-key")
    );

    // Clients ask Jev through System One's own endpoint; model lists leave it out, and it
    // does not chat.
    let r = shadow.http.get(format!("{}/v1/models", shadow.url)).bearer_auth(APP).send().await.unwrap();
    assert!(!r.text().await.unwrap().contains("jev"), "model lists leave decision models out");
    let (status, body) = shadow.openai(APP, chat("typesafe/jev-1.13.0", "hello")).await;
    assert_eq!(
        (status, body["error"]["message"].as_str()),
        (400, Some("the model \"typesafe/jev-1.13.0\" is not a chat model"))
    );
    let asked = shadow
        .http
        .post(format!("{}/v1/systemone", shadow.url))
        .bearer_auth(APP)
        .json(&serde_json::json!({
            "model": "typesafe/jev-1.13.0",
            "state": [{ "from": "user", "text": "a legal advice question" }],
            "questions": { "topic": { "type": "choice", "criteria": { "legal_advice": {}, "unknown": {} } } }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(asked.status(), 200);
    let answer: serde_json::Value = asked.json().await.unwrap();
    assert_eq!(answer["answers"]["topic"]["choice"], "legal_advice");
    shadow.server.shutdown().await;

    // Enforce with bodies off: a flagged call's bodies are not logged after all.
    let enforce = start(&[jev.as_slice(), &[("PGW_INTENT_MODE", "enforce")]].concat()).await;
    assert_eq!(enforce.openai(APP, chat("openai/gpt-test", "a jailbreak, about a billing question")).await.0, 200);
    let calls = enforce.calls(2).await;
    let (call, _, audit) = classified(&calls, 0);
    assert_eq!((call.intent.as_str(), call.plugin_verdict.as_str()), ("billing", "flag"));
    assert!(audit.is_none(), "PGW_LOG_BODIES is off");
    enforce.server.shutdown().await;
}

#[tokio::test]
async fn a_call_at_temperature_0_is_answered_again_from_memory() {
    let lite = start(&[]).await;
    let body = json!({ "model": "openai/gpt-test", "temperature": 0,
                       "messages": [{ "role": "user", "content": "the same question" }] });
    let (status, first) = lite.openai(APP, body.clone()).await;
    assert_eq!(status, 200, "{first}");
    assert_eq!(lite.last_call(1).await.cache, "miss");
    // The answer is kept off the request path, once the call has settled.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (status, again) = lite.openai(APP, body.clone()).await;
    assert_eq!(status, 200, "{again}");
    // Written as a translated answer is: the same content, without members only the provider
    // adds to its own (`refusal`, `annotations`).
    assert_eq!(again["choices"][0]["message"]["content"], first["choices"][0]["message"]["content"]);
    assert_eq!(lite.mock.captured().len(), 1, "the provider was asked once");
    let e = lite.last_call(2).await;
    assert_eq!((e.cache.as_str(), e.usage_source.as_str(), e.cost), ("hit", "cache", Some(0.0)));
    assert!(e.cache_saved_cost.is_some_and(|c| c > 0.0), "{:?}", e.cache_saved_cost);

    // Each client's answers are its own by default.
    let (status, _) = lite.openai(LIMITED, body.clone()).await;
    assert_eq!(status, 200);
    assert_eq!(lite.last_call(3).await.cache, "miss");

    // A call at another temperature is not cached.
    let warm = json!({ "model": "openai/gpt-test", "temperature": 0.7,
                       "messages": [{ "role": "user", "content": "the same question" }] });
    assert_eq!(lite.openai(APP, warm).await.0, 200);
    assert_eq!(lite.last_call(4).await.cache, "bypass");
    lite.server.shutdown().await;

    // Kept for the workspace, every client of one minimal gateway gets the same answer.
    let shared = start(&[("PGW_CACHE_SCOPE", "workspace")]).await;
    assert_eq!(shared.openai(APP, body.clone()).await.0, 200);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(shared.openai(LIMITED, body.clone()).await.0, 200);
    assert_eq!(shared.last_call(2).await.cache, "hit");
    shared.server.shutdown().await;

    // Kept per end user, a call that names none is not cached.
    let per_user = start(&[("PGW_CACHE_SCOPE", "end_user")]).await;
    let mut alice = body.clone();
    alice["user"] = json!("alice");
    assert_eq!(per_user.openai(APP, alice.clone()).await.0, 200);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(per_user.openai(APP, alice).await.0, 200);
    assert_eq!(per_user.last_call(2).await.cache, "hit");
    assert_eq!(per_user.openai(APP, body.clone()).await.0, 200);
    assert_eq!(per_user.last_call(3).await.cache, "bypass");
    per_user.server.shutdown().await;

    let off = start(&[("PGW_CACHE_MODE", "off")]).await;
    assert_eq!(off.openai(APP, body.clone()).await.0, 200);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(off.openai(APP, body).await.0, 200);
    assert_eq!(off.mock.captured().len(), 2);
    assert_eq!(off.last_call(2).await.cache, "", "no cache is bound");
    off.server.shutdown().await;
}

#[tokio::test]
async fn files_and_batches_are_kept_in_their_directory_and_accounted() {
    let dir = std::env::temp_dir().join(format!("pgw-lite-it-{}", uuid::Uuid::new_v4().simple()));
    let dir_text = dir.display().to_string();
    let lite = start(&[("PGW_OBJECTS_DIR", dir_text.as_str())]).await;
    let anthropic = |method: reqwest::Method, path: &str| {
        lite.http
            .request(method, format!("{}{path}", lite.url))
            .header("x-api-key", APP)
            .header("anthropic-version", "2023-06-01")
    };
    let boundary = "litefiles";
    let form = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.txt\"\r\nContent-Type: text/plain\r\n\r\nhello\r\n--{boundary}--\r\n"
    );
    let r = anthropic(reqwest::Method::POST, "/v1/files")
        .header("content-type", format!("multipart/form-data; boundary={boundary}"))
        .body(form)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let file: Value = r.json().await.unwrap();
    let file_id = file["id"].as_str().unwrap().to_owned();

    let batch = json!({ "requests": [{ "custom_id": "one", "params": {
        "model": "anthropic/claude-test", "max_tokens": 20, "messages": [{ "role": "user", "content": "hello" }] } }] });
    let r = anthropic(reqwest::Method::POST, "/v1/messages/batches").json(&batch).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let id = r.json::<Value>().await.unwrap()["id"].as_str().unwrap().to_owned();
    // The mock ends a batch once it has been asked about twice.
    for _ in 0..2 {
        assert_eq!(
            anthropic(reqwest::Method::GET, &format!("/v1/messages/batches/{id}")).send().await.unwrap().status(),
            200
        );
    }
    // Accounted by the gateway's own settlement, into the call log.
    let deadline = Instant::now() + Duration::from_secs(30);
    let item = loop {
        let found = lite.calls.0.lock().unwrap().iter().map(|(e, _)| e.clone()).find(|e| e.trace_id == id);
        if let Some(e) = found {
            break e;
        }
        assert!(Instant::now() < deadline, "the batch was not accounted");
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!((item.status.as_str(), item.usage_source.as_str()), ("ok", "provider"));
    assert!(item.cost.is_some_and(|c| c > 0.0), "{:?}", item.cost);
    lite.server.shutdown().await;

    // Restarted on the same directory, the gateway still knows the file and the batch.
    let again = start(&[("PGW_OBJECTS_DIR", dir_text.as_str())]).await;
    let listed: Value = again
        .http
        .get(format!("{}/v1/files", again.url))
        .header("x-api-key", APP)
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["data"][0]["id"], file_id.as_str());
    again.server.shutdown().await;
}
