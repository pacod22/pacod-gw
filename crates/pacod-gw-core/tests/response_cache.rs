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
        let mut req = self.gw.http.post(format!("{}{path}", self.gw.data)).bearer_auth(&self.fx.key).json(body);
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

/// The Valkey of `make test-services`, or the one `GW_TEST_VALKEY_URL` names.
fn valkey(b: &mut pacod_gw_core::bootstrap::Bootstrap) {
    b.cache.valkey_url =
        Some(std::env::var("GW_TEST_VALKEY_URL").unwrap_or_else(|_| "redis://127.0.0.1:26379/0".into()));
    b.cache.valkey_password = Some(std::env::var("GW_TEST_VALKEY_PASSWORD").unwrap_or_else(|_| "test".into()));
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
