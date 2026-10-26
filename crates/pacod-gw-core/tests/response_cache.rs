//! The response cache plugin (`docs/plugin-concept.md`, section 15), end to end: installed and
//! bound everywhere by default, hits in every interface and as a stream or whole, what is
//! cached, the scopes that change it, what the caller can ask for, the event of a hit, and the
//! store (memory, Valkey, a Valkey that fails).

mod common;

use std::time::Duration;

use common::{Fixture, IngestSink, TestGw, TestNode};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

struct World {
    gw: TestGw,
    mock: MockProvider,
    sink: IngestSink,
    fx: Fixture,
}

const RATES: &str = r#"{ "currency": "USD", "input": 2.0, "output": 10.0 }"#;

/// A gateway with a Claude-like, a GPT-like and a Gemini model, all priced.
async fn setup() -> World {
    setup_on(&common::fresh_database().await, |_| {}).await
}

async fn setup_on(db: &str, tweak: impl FnOnce(&mut pacod_gw_core::bootstrap::Bootstrap)) -> World {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start_on(db, Some(&sink), tweak).await;
    let fx = gw.fixture("anthropic", &mock.base_url()).await;
    let rates: Value = serde_json::from_str(RATES).unwrap();
    gw.priced_model(
        &fx,
        "anthropic/claude",
        "claude-sonnet-5-5",
        json!({ "capability_preset": "anthropic-adaptive", "max_output_tokens": 16000 }),
        rates.clone(),
    )
    .await;
    let openai = gw
        .create(
            "/providers",
            json!({ "name": "openai", "family": "openai", "base_url": format!("{}/v1", mock.base_url()) }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{openai}/credentials"), json!({ "secret": "sk-openai-side" })).await;
    let gemini = gw
        .create(
            "/providers",
            json!({ "name": "gemini", "family": "gemini", "managed_api": "native",
                    "base_url": format!("{}/emulate/gemini/v1beta", mock.base_url()) }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{gemini}/credentials"), json!({ "secret": "gm-key" })).await;
    for (p, name, upstream) in [(&openai, "openai/gpt", "gpt-x"), (&gemini, "gemini/flash", "gemini-3.8-flash")] {
        let id = gw.draft_model(p, name, upstream, json!({})).await;
        let (st, v) = gw.api("PUT", &format!("/models/{id}/price"), Some(rates.clone())).await;
        assert_eq!(st, 201, "{v}");
        gw.activate(&id).await;
        gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    }
    World { gw, mock, sink, fx }
}

struct Reply {
    status: u16,
    headers: reqwest::header::HeaderMap,
    text: String,
}

impl Reply {
    fn header(&self, name: &str) -> &str {
        self.headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or_default()
    }

    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text))
    }

    /// The `data:` payloads of a stream of server-sent events.
    fn data(&self) -> Vec<String> {
        self.text.lines().filter_map(|l| l.strip_prefix("data: ")).map(str::to_owned).collect()
    }
}

fn openai_body(text: &str, temperature: f64) -> Value {
    json!({ "model": "openai/gpt", "temperature": temperature, "messages": [{ "role": "user", "content": text }] })
}

impl World {
    fn ws(&self, rest: &str) -> String {
        format!("/orgs/{}/workspaces/{}{rest}", self.fx.org, self.fx.ws)
    }

    async fn post(&self, path: &str, body: &Value, headers: &[(&str, &str)]) -> Reply {
        self.post_as(&self.fx.key, path, body, headers).await
    }

    async fn post_as(&self, key: &str, path: &str, body: &Value, headers: &[(&str, &str)]) -> Reply {
        let mut req = self.gw.http.post(format!("{}{path}", self.gw.data)).bearer_auth(key).json(body);
        for (n, v) in headers {
            req = req.header(*n, *v);
        }
        let r = req.send().await.unwrap();
        Reply { status: r.status().as_u16(), headers: r.headers().clone(), text: r.text().await.unwrap() }
    }

    async fn chat(&self, body: &Value) -> Reply {
        self.post("/v1/chat/completions", body, &[]).await
    }

    /// Requests the provider received at a path ending in `suffix`.
    fn asked(&self, suffix: &str) -> usize {
        self.mock.captured().iter().filter(|c| c.path.ends_with(suffix)).count()
    }

    /// Waits until `n` calls have settled, and a moment more: an answer is kept off the
    /// request path, after its call settled.
    async fn settled(&self, n: usize) -> Vec<pacod_gw_events::UsageEvent> {
        let events = self.sink.wait_events(n).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        events
    }

    async fn event(&self, reply: &Reply, n: usize) -> pacod_gw_events::UsageEvent {
        let id = reply.header("x-gw-request-id").trim_start_matches("gw_").to_owned();
        let events = self.sink.wait_events(n).await;
        events.into_iter().find(|e| e.id.simple().to_string() == id).expect("the call's event")
    }
}

fn content(r: &Reply) -> String {
    r.json()["choices"][0]["message"]["content"].as_str().unwrap_or_default().to_owned()
}

// --- the tests ---------------------------------------------------------------------------------

#[tokio::test]
async fn the_cache_is_installed_and_bound_everywhere_by_default() {
    let w = setup().await;
    let (st, v) = w.gw.api("GET", "/plugins", None).await;
    assert_eq!(st, 200, "{v}");
    let cache = v.as_array().unwrap().iter().find(|p| p["id"] == "response_cache").expect("listed").clone();
    assert_eq!(
        (cache["installed"].clone(), cache["enabled"].clone(), cache["installed_by_default"].clone()),
        (json!(true), json!(true), json!(true))
    );
    assert_eq!(cache["hooks"], json!(["answers"]));
    let (st, v) = w.gw.api("GET", "/plugin-installs", None).await;
    assert_eq!(st, 200, "{v}");
    let install = v["items"].as_array().unwrap().iter().find(|i| i["plugin"] == "response_cache").unwrap().clone();
    assert_eq!(install["default_binding"]["mode"], "enforce");
    assert_eq!(install["default_binding"]["settings"]["calls"], "deterministic");

    // The cache checks nothing: a workspace that has only the cache is not a workspace that
    // enforces a plugin, so blind relay is not refused.
    let blind = w.gw.key(&w.fx, json!({ "name": "blind", "inspect": "none", "usage_policy": "ignore" })).await;
    w.gw.create(
        &format!("/providers/{}/credentials", w.fx.provider),
        json!({ "label": "own", "secret": "sk-own", "scope": { "type": "workspace", "id": w.fx.ws } }),
    )
    .await;
    let r = w
        .gw
        .http
        .post(format!("{}/proxy/anthropic/v1/messages", w.gw.data))
        .bearer_auth(&blind)
        .header("content-type", "application/json")
        .body(
            json!({ "model": "claude-sonnet-5-5", "max_tokens": 10, "messages": [{ "role": "user", "content": "x" }] })
                .to_string(),
        )
        .send()
        .await
        .unwrap();
    // What the provider makes of the body does not matter here; a workspace that enforces a
    // plugin would refuse the relay with 403 before sending it.
    assert_ne!(r.status(), 403, "blind relay is not refused");
}

#[tokio::test]
async fn a_repeated_deterministic_call_is_answered_from_the_cache() {
    let w = setup().await;
    let body = openai_body("what is the capital of Norway", 0.0);
    let first = w.chat(&body).await;
    assert_eq!(first.status, 200, "{}", first.text);
    assert_eq!(first.header("x-gw-cache"), "miss");
    w.settled(1).await;

    let second = w.chat(&body).await;
    assert_eq!(second.status, 200, "{}", second.text);
    assert_eq!(second.header("x-gw-cache"), "hit");
    assert!(second.header("age").parse::<u64>().is_ok(), "an Age header");
    assert_eq!(second.header("x-gw-attempts"), "0");
    assert_eq!(second.header("x-gw-model-served"), "openai/gpt");
    assert_eq!(content(&second), content(&first));
    assert_eq!(second.json()["usage"]["cost"]["amount"], json!(0.0), "a hit costs nothing");
    assert_eq!(w.asked("/chat/completions"), 1, "the provider was asked once");

    let e = w.event(&second, 2).await;
    assert_eq!((e.cache.as_str(), e.usage_source.as_str(), e.status.as_str()), ("hit", "cache", "ok"));
    assert_eq!((e.attempts, e.cost, e.usage.output_tokens), (0, Some(0.0), 0));
    assert!(e.cache_saved_cost.is_some_and(|c| c > 0.0), "what the answer would have cost: {:?}", e.cache_saved_cost);
    // The price is in the reporting currency: converted as it is.
    assert_eq!(e.cache_saved_cost_reporting, e.cache_saved_cost);
    let first_event = w.event(&first, 2).await;
    assert_eq!(first_event.cache, "miss");
    assert!(first_event.cost.is_some_and(|c| c > 0.0));

    // Another question, and another workspace, are other calls.
    let r = w.chat(&openai_body("what is the capital of Sweden", 0.0)).await;
    assert_eq!(r.header("x-gw-cache"), "miss");
}

#[tokio::test]
async fn a_stream_and_a_whole_answer_share_one_kept_answer() {
    let w = setup().await;
    // Whole first, then a stream of the same request.
    let mut body = openai_body("tell me about fjords", 0.0);
    let whole = w.chat(&body).await;
    assert_eq!(whole.header("x-gw-cache"), "miss");
    w.settled(1).await;
    body["stream"] = json!(true);
    body["stream_options"] = json!({ "include_usage": true });
    let stream = w.chat(&body).await;
    assert_eq!((stream.status, stream.header("x-gw-cache")), (200, "hit"), "{}", stream.text);
    assert!(stream.header("content-type").starts_with("text/event-stream"));
    let data = stream.data();
    assert_eq!(data.last().map(String::as_str), Some("[DONE]"));
    let text: String = data
        .iter()
        .filter_map(|d| serde_json::from_str::<Value>(d).ok())
        .filter_map(|c| c["choices"][0]["delta"]["content"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(text, content(&whole));
    assert_eq!(w.asked("/chat/completions"), 1);

    // A stream first, on the Anthropic interface, then the whole answer. A model that takes a
    // temperature: one that thinks (anthropic/claude) leaves it out, and is not deterministic.
    let claude = |stream: bool| {
        json!({ "model": "anthropic/claude-test", "max_tokens": 200, "temperature": 0, "stream": stream,
                "messages": [{ "role": "user", "content": "describe a glacier" }] })
    };
    let first = w.post("/v1/messages", &claude(true), &[]).await;
    assert_eq!((first.status, first.header("x-gw-cache")), (200, "miss"), "{}", first.text);
    w.settled(3).await;
    let again = w.post("/v1/messages", &claude(false), &[]).await;
    assert_eq!((again.status, again.header("x-gw-cache")), (200, "hit"), "{}", again.text);
    let v = again.json();
    assert_eq!(v["type"], "message");
    let streamed: String = first
        .data()
        .iter()
        .filter_map(|d| serde_json::from_str::<Value>(d).ok())
        .filter(|e| e["type"] == "content_block_delta")
        .filter_map(|e| e["delta"]["text"].as_str().map(str::to_owned))
        .collect();
    assert_eq!(v["content"][0]["text"].as_str().unwrap_or_default(), streamed);
    assert_eq!(w.asked("/v1/messages"), 1);
}

#[tokio::test]
async fn a_responses_caller_is_answered_from_the_cache_whole_and_streamed() {
    let w = setup().await;
    let body = json!({ "model": "anthropic/claude", "temperature": 0, "input": "hello" });
    let first = w.post("/v1/responses", &body, &[]).await;
    assert_eq!((first.status, first.header("x-gw-cache")), (200, "miss"), "{}", first.text);
    w.settled(1).await;
    let hit = w.post("/v1/responses", &body, &[]).await;
    assert_eq!((hit.status, hit.header("x-gw-cache")), (200, "hit"), "{}", hit.text);
    assert_eq!(hit.json()["output"], first.json()["output"]);
    assert_eq!(hit.json()["object"], "response");

    let mut streamed = body.clone();
    streamed["stream"] = json!(true);
    let hit = w.post("/v1/responses", &streamed, &[]).await;
    assert_eq!((hit.status, hit.header("x-gw-cache")), (200, "hit"), "{}", hit.text);
    let last: Value = serde_json::from_str(hit.data().last().unwrap()).unwrap();
    assert_eq!(last["type"], "response.completed");
    assert_eq!(last["response"]["output"], first.json()["output"]);
    assert_eq!(w.asked("/v1/messages"), 1, "Claude answered once");
}

#[tokio::test]
async fn every_interface_reads_a_kept_answer_in_its_own_dialect() {
    let w = setup().await;
    // The native interface: the answer carries this call's id and gateway block.
    let native = json!({ "model": "anthropic/claude-test", "max_output_tokens": 100, "temperature": 0,
                         "messages": [{ "role": "user", "content": [{ "type": "text", "text": "hello" }] }] });
    let first = w.post("/gw/v1/chat", &native, &[]).await;
    assert_eq!((first.status, first.header("x-gw-cache")), (200, "miss"), "{}", first.text);
    w.settled(1).await;
    let hit = w.post("/gw/v1/chat", &native, &[]).await;
    assert_eq!((hit.status, hit.header("x-gw-cache")), (200, "hit"), "{}", hit.text);
    let v = hit.json();
    assert_eq!(v["id"].as_str(), Some(hit.header("x-gw-request-id")), "the call's own id");
    assert_eq!(v["gateway"]["request_id"].as_str(), Some(hit.header("x-gw-request-id")));
    assert_eq!(v["content"], first.json()["content"]);

    // Gemini's interface, on a Gemini model.
    let gemini = json!({ "contents": [{ "role": "user", "parts": [{ "text": "hello" }] }],
                         "generationConfig": { "temperature": 0 } });
    let path = "/v1beta/models/gemini/flash:generateContent";
    let send = || async {
        let r = w.gw.http.post(format!("{}{path}", w.gw.data)).header("x-goog-api-key", &w.fx.key).json(&gemini);
        let r = r.send().await.unwrap();
        Reply { status: r.status().as_u16(), headers: r.headers().clone(), text: r.text().await.unwrap() }
    };
    let first = send().await;
    assert_eq!((first.status, first.header("x-gw-cache")), (200, "miss"), "{}", first.text);
    w.settled(3).await;
    let hit = send().await;
    assert_eq!((hit.status, hit.header("x-gw-cache")), (200, "hit"), "{}", hit.text);
    assert_eq!(hit.json()["candidates"][0]["content"], first.json()["candidates"][0]["content"]);
}

#[tokio::test]
async fn only_deterministic_calls_by_default_and_all_when_bound_so() {
    let w = setup().await;
    let warm = openai_body("write a poem", 0.8);
    assert_eq!(w.chat(&warm).await.header("x-gw-cache"), "bypass");
    w.settled(1).await;
    assert_eq!(w.chat(&warm).await.header("x-gw-cache"), "bypass");
    assert_eq!(w.asked("/chat/completions"), 2);

    w.gw.create(
        &w.ws("/plugin-bindings"),
        json!({ "plugin": "response_cache", "mode": "enforce", "settings": { "calls": "all", "ttl_seconds": 60 } }),
    )
    .await;
    w.gw.wait_applied().await;
    assert_eq!(w.chat(&warm).await.header("x-gw-cache"), "miss");
    w.settled(3).await;
    assert_eq!(w.chat(&warm).await.header("x-gw-cache"), "hit");
    assert_eq!(w.asked("/chat/completions"), 3);
}

#[tokio::test]
async fn organisations_and_workspaces_change_the_platforms_binding() {
    let w = setup().await;
    let body = openai_body("hello", 0.0);
    // The organisation switches the cache off for its workspaces.
    let org =
        w.gw.create(
            &format!("/orgs/{}/plugin-bindings", w.fx.org),
            json!({ "plugin": "response_cache", "mode": "off", "settings": {} }),
        )
        .await
        .id;
    w.gw.wait_applied().await;
    let r = w.chat(&body).await;
    assert_eq!((r.status, r.header("x-gw-cache")), (200, ""), "no cache is bound");

    // A workspace binds it again for itself.
    w.gw.create(&w.ws("/plugin-bindings"), json!({ "plugin": "response_cache", "mode": "enforce", "settings": {} }))
        .await;
    w.gw.wait_applied().await;
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "miss");

    // A locked organisation binding wins over the workspace's.
    let (st, v) =
        w.gw.api(
            "PUT",
            &format!("/orgs/{}/plugin-bindings/{org}", w.fx.org),
            Some(json!({ "plugin": "response_cache", "mode": "off", "locked": true, "settings": {} })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    w.gw.wait_applied().await;
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "");

    // Without the organisation's binding the workspace's applies again.
    let (st, v) = w.gw.api("DELETE", &format!("/orgs/{}/plugin-bindings/{org}", w.fx.org), None).await;
    assert_eq!(st, 204, "{v}");
    w.gw.wait_applied().await;
    assert_ne!(w.chat(&body).await.header("x-gw-cache"), "");

    // The platform switches it off everywhere, whatever the bindings say.
    let (_, v) = w.gw.api("GET", "/plugin-installs", None).await;
    let install = v["items"].as_array().unwrap().iter().find(|i| i["plugin"] == "response_cache").unwrap().clone();
    let id = install["id"].as_str().unwrap().to_owned();
    let mut off = install.clone();
    off["enabled"] = json!(false);
    for k in ["id", "version", "created_at", "updated_at"] {
        off.as_object_mut().unwrap().remove(k);
    }
    let (st, v) = w.gw.api("PUT", &format!("/plugin-installs/{id}"), Some(off)).await;
    assert_eq!(st, 200, "{v}");
    w.gw.wait_applied().await;
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "");
}

#[tokio::test]
async fn answers_are_kept_per_key_unless_the_scope_says_otherwise() {
    let w = setup().await;
    let other = w.gw.key(&w.fx, json!({ "name": "other" })).await;
    const CHAT: &str = "/v1/chat/completions";
    let body = openai_body("whose answer is this", 0.0);
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "miss");
    w.settled(1).await;
    assert_eq!(w.post_as(&other, CHAT, &body, &[]).await.header("x-gw-cache"), "miss", "another key");
    w.settled(2).await;
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "hit", "the key's own answer");

    // Kept for the workspace, every key gets the answer.
    let binding =
        w.gw.create(
            &w.ws("/plugin-bindings"),
            json!({ "plugin": "response_cache", "mode": "enforce", "settings": { "scope": "workspace" } }),
        )
        .await
        .id;
    w.gw.wait_applied().await;
    let body = openai_body("an answer for everyone", 0.0);
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "miss");
    w.settled(4).await;
    assert_eq!(w.post_as(&other, CHAT, &body, &[]).await.header("x-gw-cache"), "hit");

    // Kept per end user, an end user gets only their own answers, and a call that names none
    // is not cached.
    let (st, v) =
        w.gw.api(
            "PUT",
            &w.ws(&format!("/plugin-bindings/{binding}")),
            Some(json!({ "plugin": "response_cache", "mode": "enforce", "settings": { "scope": "end_user" } })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    w.gw.wait_applied().await;
    let body = openai_body("an answer for one person", 0.0);
    let as_user = |user: &'static str| [("x-gw-end-user", user)];
    assert_eq!(w.post(CHAT, &body, &as_user("alice")).await.header("x-gw-cache"), "miss");
    w.settled(6).await;
    assert_eq!(w.post(CHAT, &body, &as_user("alice")).await.header("x-gw-cache"), "hit");
    assert_eq!(w.post(CHAT, &body, &as_user("bob")).await.header("x-gw-cache"), "miss", "another end user");
    assert_eq!(w.post_as(&other, CHAT, &body, &as_user("alice")).await.header("x-gw-cache"), "miss", "another key");
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "bypass", "no end user");
}

#[tokio::test]
async fn the_caller_can_bypass_or_refresh_the_cache() {
    let w = setup().await;
    let body = openai_body("good morning", 0.0);
    assert_eq!(w.post("/v1/chat/completions", &body, &[("x-gw-cache", "off")]).await.header("x-gw-cache"), "bypass");
    w.settled(1).await;
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "miss", "a bypassed answer is not kept");
    w.settled(2).await;
    assert_eq!(
        w.post("/v1/chat/completions", &body, &[("x-gw-cache", "refresh")]).await.header("x-gw-cache"),
        "refresh"
    );
    w.settled(3).await;
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "hit");
    assert_eq!(w.asked("/chat/completions"), 3);

    // The body's gateway object says the same on the native interface.
    let native = json!({ "model": "openai/gpt", "temperature": 0, "gateway": { "cache": { "response": "off" } },
                         "messages": [{ "role": "user", "content": [{ "type": "text", "text": "good morning" }] }] });
    assert_eq!(w.post("/gw/v1/chat", &native, &[]).await.header("x-gw-cache"), "bypass");
    let bad = w.post("/v1/chat/completions", &body, &[("x-gw-cache", "sometimes")]).await;
    assert_eq!(bad.status, 400, "{}", bad.text);
}

#[tokio::test]
async fn shadow_records_what_it_would_have_answered() {
    let w = setup().await;
    w.gw.create(&w.ws("/plugin-bindings"), json!({ "plugin": "response_cache", "mode": "shadow", "settings": {} }))
        .await;
    w.gw.wait_applied().await;
    let body = openai_body("hello there", 0.0);
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "miss");
    w.settled(1).await;
    let r = w.chat(&body).await;
    assert_eq!(r.header("x-gw-cache"), "would_hit");
    assert_eq!(w.asked("/chat/completions"), 2, "the provider answered all the same");
    let e = w.event(&r, 2).await;
    assert_eq!((e.cache.as_str(), e.usage_source.as_str()), ("would_hit", "provider"));
}

#[tokio::test]
async fn a_failing_store_is_a_miss_and_never_fails_a_call() {
    // Nothing listens on port 1.
    let db = common::fresh_database().await;
    let w = setup_on(&db, |b| b.cache.valkey_url = Some("redis://127.0.0.1:1/0".into())).await;
    let body = openai_body("are you there", 0.0);
    for _ in 0..5 {
        let r = w.chat(&body).await;
        assert_eq!((r.status, r.header("x-gw-cache")), (200, "miss"), "{}", r.text);
    }
    assert_eq!(w.asked("/chat/completions"), 5);
}

/// The Valkey of `make test-services`, or the one `GW_TEST_VALKEY_URL` names; for one with TLS
/// (`rediss://`), `GW_TEST_VALKEY_CA_FILE` names its certificate's authority.
fn valkey(b: &mut pacod_gw_core::bootstrap::Bootstrap) {
    b.cache.valkey_url =
        Some(std::env::var("GW_TEST_VALKEY_URL").unwrap_or_else(|_| "redis://127.0.0.1:26379/0".into()));
    b.cache.valkey_password = Some(std::env::var("GW_TEST_VALKEY_PASSWORD").unwrap_or_else(|_| "test".into()));
    b.cache.valkey_ca_file = std::env::var("GW_TEST_VALKEY_CA_FILE").ok().map(Into::into);
}

#[tokio::test]
async fn valkey_shares_kept_answers_between_instances() {
    let db = common::fresh_database().await;
    let w = setup_on(&db, valkey).await;
    let node = TestNode::start(&db, valkey).await;
    let body = openai_body("shared between machines", 0.0);
    let first = w.chat(&body).await;
    assert_eq!((first.status, first.header("x-gw-cache")), (200, "miss"), "{}", first.text);
    w.settled(1).await;
    let r = w.gw.http.post(format!("{}/v1/chat/completions", node.data)).bearer_auth(&w.fx.key).json(&body);
    let r = r.send().await.unwrap();
    assert_eq!(r.headers().get("x-gw-cache").and_then(|v| v.to_str().ok()), Some("hit"), "the other instance");
    assert_eq!(w.asked("/chat/completions"), 1);
}

#[tokio::test]
async fn the_install_is_made_once_and_stays_deleted() {
    let db = common::fresh_database().await;
    let w = setup_on(&db, |_| {}).await;
    let (_, v) = w.gw.api("GET", "/plugin-installs", None).await;
    let id = v["items"].as_array().unwrap().iter().find(|i| i["plugin"] == "response_cache").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (st, v) = w.gw.api("DELETE", &format!("/plugin-installs/{id}"), None).await;
    assert_eq!(st, 204, "{v}");
    // Another start on the same database does not install it again.
    let again = TestGw::start_on(&db, None, |_| {}).await;
    let (_, v) = again.api("GET", "/plugin-installs", None).await;
    assert!(v["items"].as_array().unwrap().iter().all(|i| i["plugin"] != "response_cache"), "{v}");
}

// --- clearing and the status ---------------------------------------------------------------------

impl World {
    async fn clear(&self, path: &str) -> Value {
        let (st, v) = self.gw.api("POST", &format!("{path}/response-cache/clear"), None).await;
        assert_eq!(st, 200, "{v}");
        v
    }

    fn org_path(&self) -> String {
        format!("/orgs/{}", self.fx.org)
    }

    fn ws_path(&self) -> String {
        format!("/orgs/{}/workspaces/{}", self.fx.org, self.fx.ws)
    }
}

/// The figures an instance published, once `ok` holds for them.
async fn figures_until(gw: &TestGw, instance: &str, ok: impl Fn(&Value) -> bool) -> Value {
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let (st, v) = gw.api("GET", "/response-cache", None).await;
        assert_eq!(st, 200, "{v}");
        let i = v["instances"].as_array().unwrap().iter().find(|i| i["id"] == instance).cloned();
        if let Some(f) = i.map(|i| i["figures"].clone()).filter(|f| ok(f)) {
            return f;
        }
        assert!(std::time::Instant::now() < deadline, "the figures of {instance} never matched: {v}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn a_clear_makes_kept_answers_misses() {
    let w = setup().await;
    let body = openai_body("what is the capital of Peru", 0.0);
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "miss");
    w.settled(1).await;
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "hit");

    let (st, v) = w.gw.api("GET", &format!("{}/response-cache", w.ws_path()), None).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!((v["store"].as_str(), v["binding"]["source"].as_str()), (Some("memory"), Some("platform")), "{v}");
    assert_eq!(v["binding"]["settings"]["plugin"], "response_cache");
    assert!(v["cleared"]["effective"].is_null());

    let r = w.clear(&w.ws_path()).await;
    assert_eq!((r["scope"].as_str(), r["reclaim"].as_str()), (Some("workspace"), Some("per_instance")), "{r}");
    assert!(r["config_version"].is_i64());
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "miss", "kept before the clear");
    assert_eq!(w.asked("/chat/completions"), 2);

    // An organisation's clear and the platform's reach the workspace too, and show where.
    w.clear(&w.org_path()).await;
    w.clear("").await;
    let (_, v) = w.gw.api("GET", &format!("{}/response-cache", w.ws_path()), None).await;
    for scope in ["own", "organisation", "platform"] {
        assert!(v["cleared"][scope]["at"].is_string(), "{scope}: {v}");
    }
    assert_eq!(v["cleared"]["effective"], v["cleared"]["platform"]["at"]);
    assert_eq!(v["cleared"]["platform"]["by_display"], "admin@example.test", "{v}");
    let (_, v) = w.gw.api("GET", "/response-cache", None).await;
    let scopes: Vec<&str> =
        v["recent_clears"].as_array().unwrap().iter().map(|c| c["scope"].as_str().unwrap()).collect();
    assert_eq!(scopes, ["platform", "organisation", "workspace"], "newest first: {v}");
    assert_eq!(v["install"]["installed"], true);
    assert_eq!(v["install"]["default_binding"]["mode"], "enforce");
    let (_, log) = w.gw.api("GET", "/audit-log?limit=10", None).await;
    let clears = log["items"].as_array().unwrap().iter().filter(|e| e["action"] == "clear").count();
    assert_eq!(clears, 3, "audited: {log}");
}

#[tokio::test]
async fn a_clear_reaches_the_memory_of_every_instance() {
    let db = common::fresh_database().await;
    let w = setup_on(&db, |_| {}).await;
    let node = TestNode::start(&db, |_| {}).await;
    let body = openai_body("kept on the other machine", 0.0);
    let call = || async {
        let r = w.gw.http.post(format!("{}/v1/chat/completions", node.data)).bearer_auth(&w.fx.key).json(&body);
        let r = r.send().await.unwrap();
        r.headers().get("x-gw-cache").and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned()
    };
    assert_eq!(call().await, "miss");
    // The node sends no events here: its figures say when the answer is kept.
    let f = figures_until(&w.gw, node.instance_id(), |f| f["entries"].as_u64().is_some_and(|n| n >= 1)).await;
    assert_eq!(f["store"], "memory");
    assert_eq!(call().await, "hit");

    w.clear(&w.ws_path()).await;
    w.gw.wait_applied().await;
    figures_until(&w.gw, node.instance_id(), |f| f["entries"] == 0).await;
    assert_eq!(call().await, "miss", "the other instance applied the clear");
    let (_, v) = w.gw.api("GET", "/response-cache", None).await;
    assert_eq!(v["store"], "memory");
    assert!(v["valkey"].is_null() && v.get("valkey_note").is_none(), "{v}");
}

#[tokio::test]
async fn clearing_is_for_administrators_of_the_scope() {
    let w = setup().await;
    let user = |email: &'static str, role: &'static str| {
        let w = &w;
        async move {
            let id = w.gw.create("/users", json!({ "email": email, "password": "a long password 1" })).await.id;
            w.gw.create(
                "/role-bindings",
                json!({ "principal": id, "role": role, "scope": { "type": "workspace", "id": w.fx.ws } }),
            )
            .await;
            w.gw.login(email, "a long password 1").await
        }
    };
    let viewer = user("vic@example.test", "viewer").await;
    let admin = user("wanda@example.test", "workspace_admin").await;
    let ws = format!("{}/response-cache", w.ws_path());
    assert_eq!(w.gw.api_as(&viewer, "GET", &ws, None).await.0, 200);
    assert_eq!(w.gw.api_as(&viewer, "POST", &format!("{ws}/clear"), None).await.0, 403);
    assert_eq!(w.gw.api_as(&viewer, "GET", "/response-cache", None).await.0, 403);
    assert_eq!(w.gw.api_as(&admin, "POST", &format!("{ws}/clear"), None).await.0, 200);
    assert_eq!(w.gw.api_as(&admin, "POST", &format!("{}/response-cache/clear", w.org_path()), None).await.0, 403);
    assert_eq!(w.gw.api_as(&admin, "POST", "/response-cache/clear", None).await.0, 403);
    let other = format!("/orgs/{}/workspaces/ws_nope/response-cache", w.fx.org);
    assert_eq!(w.gw.api("GET", &other, None).await.0, 404);
}

#[tokio::test]
async fn a_cleared_workspace_or_organisation_can_still_be_deleted() {
    let w = setup().await;
    let org = w.gw.create("/orgs", json!({ "name": "spare" })).await.id;
    let ws = w.gw.create(&format!("/orgs/{org}/workspaces"), json!({ "name": "spare" })).await.id;
    w.clear(&format!("/orgs/{org}/workspaces/{ws}")).await;
    w.clear(&format!("/orgs/{org}")).await;
    let (st, v) = w.gw.api("DELETE", &format!("/orgs/{org}/workspaces/{ws}"), None).await;
    assert_eq!(st, 204, "{v}");
    let (st, v) = w.gw.api("DELETE", &format!("/orgs/{org}"), None).await;
    assert_eq!(st, 204, "{v}");
}

#[tokio::test]
async fn the_status_tells_why_a_workspace_has_no_cache() {
    let w = setup().await;
    let path = format!("{}/response-cache", w.ws_path());
    let (st, v) =
        w.gw.api("POST", &w.ws("/plugin-bindings"), Some(json!({ "plugin": "response_cache", "mode": "off" }))).await;
    assert_eq!(st, 201, "{v}");
    w.gw.wait_applied().await;
    let (_, v) = w.gw.api("GET", &path, None).await;
    assert!(v["binding"].is_null(), "{v}");
    assert_eq!(v["why_none"], "switched off for this workspace");
    let (_, v) = w.gw.api("GET", &format!("{}/response-cache", w.org_path()), None).await;
    assert_eq!(v["binding"]["source"], "platform", "{v}");
}

#[tokio::test]
async fn the_audit_rule_forbid_switches_the_cache_off() {
    let w = setup().await;
    let body = openai_body("nothing of this is kept", 0.0);
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "miss");
    w.settled(1).await;
    // A binding of the workspace's own does not change it.
    w.gw.create(&w.ws("/plugin-bindings"), json!({ "plugin": "response_cache", "mode": "enforce", "settings": {} }))
        .await;

    // The organisation forbids keeping its workspaces' calls: no answer is looked up or kept.
    let (_, org) = w.gw.api("GET", &w.org_path(), None).await;
    let rule = |rule: Value| json!({ "name": org["name"], "settings": { "audit": { "rule": rule } } });
    let (st, v) = w.gw.api("PUT", &w.org_path(), Some(rule(json!("forbid")))).await;
    assert_eq!(st, 200, "{v}");
    w.gw.wait_applied().await;
    let r = w.chat(&body).await;
    assert_eq!((r.status, r.header("x-gw-cache")), (200, ""), "not looked up: {}", r.text);
    for path in [w.ws_path(), w.org_path()] {
        let (_, v) = w.gw.api("GET", &format!("{path}/response-cache"), None).await;
        assert!(v["binding"].is_null(), "{v}");
        assert!(v["why_none"].as_str().unwrap().contains("organisation's audit rule forbid"), "{v}");
    }
    let (_, v) = w.gw.api("GET", "/response-cache", None).await;
    assert!(v["forbidden"].is_null(), "only the organisation forbids: {v}");

    // The platform's rule reaches every organisation, and the platform's status says so.
    let (st, v) = w.gw.api("PUT", &w.org_path(), Some(rule(Value::Null))).await;
    assert_eq!(st, 200, "{v}");
    let (_, mut settings) = w.gw.api("GET", "/settings", None).await;
    settings["audit_rule"] = json!("forbid");
    let (st, v) = w.gw.api("PUT", "/settings", Some(settings)).await;
    assert_eq!(st, 200, "{v}");
    w.gw.wait_applied().await;
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "");
    let (_, v) = w.gw.api("GET", &format!("{}/response-cache", w.ws_path()), None).await;
    assert!(v["why_none"].as_str().unwrap().contains("platform's audit rule forbid"), "{v}");
    let (_, v) = w.gw.api("GET", "/response-cache", None).await;
    assert!(v["forbidden"].as_str().is_some_and(|s| s.contains("forbid")), "{v}");
    assert_eq!(w.asked("/chat/completions"), 3, "every call reached the provider");
}

#[tokio::test]
async fn valkey_forgets_the_answers_of_a_cleared_workspace() {
    let db = common::fresh_database().await;
    let w = setup_on(&db, valkey).await;
    let body = openai_body("forgotten in valkey", 0.0);
    assert_eq!(w.chat(&body).await.header("x-gw-cache"), "miss");
    w.settled(1).await;
    let pattern = format!("pgw:answer:*:{}:*", w.fx.ws);
    assert_eq!(valkey_keys(&pattern).await.len(), 1);

    let (_, v) = w.gw.api("GET", "/response-cache", None).await;
    assert_eq!(v["valkey"]["reachable"], true, "{v}");
    assert!(v["valkey"]["keys"].as_u64().is_some_and(|n| n >= 1), "{v}");
    assert_eq!(v["store"], "valkey");

    let r = w.clear(&w.ws_path()).await;
    assert_eq!(r["reclaim"], "started");
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !valkey_keys(&pattern).await.is_empty() {
        assert!(std::time::Instant::now() < deadline, "the key was never removed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// The keys of the test Valkey that match `pattern`.
async fn valkey_conn() -> redis::aio::MultiplexedConnection {
    use redis::IntoConnectionInfo;
    let mut boot = pacod_gw_core::bootstrap::Bootstrap::for_tests("postgres://unused", common::temp_dir("valkey"));
    valkey(&mut boot);
    let mut info = boot.cache.valkey_url.unwrap().into_connection_info().unwrap();
    let settings = info.redis_settings().clone().set_password(boot.cache.valkey_password.unwrap());
    info = info.set_redis_settings(settings);
    let client = match boot.cache.valkey_ca_file {
        Some(ca) => {
            let root_cert = Some(std::fs::read(ca).unwrap());
            redis::Client::build_with_tls(info, redis::TlsCertificates { client_tls: None, root_cert }).unwrap()
        }
        None => redis::Client::open(info).unwrap(),
    };
    client.get_multiplexed_async_connection().await.unwrap()
}

async fn valkey_keys(pattern: &str) -> Vec<String> {
    redis::cmd("KEYS").arg(pattern).query_async(&mut valkey_conn().await).await.unwrap()
}

#[tokio::test]
async fn valkey_holds_answers_sealed_under_names_that_say_nothing() {
    let db = common::fresh_database().await;
    let w = setup_on(&db, valkey).await;
    let first = w.chat(&openai_body("a secret kept in valkey", 0.0)).await;
    assert_eq!(first.header("x-gw-cache"), "miss");
    w.settled(1).await;
    let keys = valkey_keys(&format!("pgw:answer:*:{}:*", w.fx.ws)).await;
    assert_eq!(keys.len(), 1, "{keys:?}");
    assert!(keys[0].starts_with(&format!("pgw:answer:v2:{}:", w.fx.ws)), "{}", keys[0]);
    let stored: Vec<u8> = redis::cmd("GET").arg(&keys[0]).query_async(&mut valkey_conn().await).await.unwrap();
    let text = String::from_utf8_lossy(&stored);
    for plain in [content(&first).as_str(), "secret kept", "openai/gpt", "call_id"] {
        assert!(!text.contains(plain), "{plain:?} is in the store");
    }
    assert_eq!(w.chat(&openai_body("a secret kept in valkey", 0.0)).await.header("x-gw-cache"), "hit");
}

#[tokio::test]
async fn a_clear_holds_while_valkey_fails() {
    let db = common::fresh_database().await;
    let w = setup_on(&db, |b| b.cache.valkey_url = Some("redis://127.0.0.1:1/0".into())).await;
    let r = w.clear(&w.ws_path()).await;
    assert_eq!(r["scope"], "workspace");
    let (st, v) = w.gw.api("GET", "/response-cache", None).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["valkey"]["reachable"], false, "{v}");
    assert!(v["valkey"]["error"].is_string());
}

#[tokio::test]
async fn a_clear_says_when_it_cannot_reach_the_gateways_valkey() {
    let db = common::fresh_database().await;
    let w = setup_on(&db, |_| {}).await;
    let node = TestNode::start(&db, valkey).await;
    figures_until(&w.gw, node.instance_id(), |f| f["store"] == "valkey").await;
    let r = w.clear(&w.ws_path()).await;
    assert_eq!(r["reclaim"], "not_available", "{r}");
    let (_, v) = w.gw.api("GET", "/response-cache", None).await;
    assert_eq!(v["store"], "mixed", "this instance keeps its own memory, the node Valkey: {v}");
    assert!(v["valkey_note"].is_string(), "{v}");
}
