//! The OpenAI-compatible providers (concept section 8.1): DeepSeek, GLM, Kimi and Ollama, each
//! set up from its preset against the mock's emulation of it, end to end through all three
//! interfaces. The emulations refuse what the real providers refuse (reasoning not sent back,
//! fixed samplers), so a passing call here is one the provider would take.

mod common;

use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_testkit::mock::{Behavior, MockProvider};
use serde_json::{Value, json};

struct World {
    gw: TestGw,
    mock: MockProvider,
    sink: IngestSink,
    fx: Fixture,
}

/// Creates a provider from a built-in preset, with its base URL at the mock's emulation and
/// `quirks` laid over the preset's.
async fn provider(gw: &TestGw, preset: &str, name: &str, base_url: &str, quirks: Value) -> String {
    let (st, presets) = gw.api("GET", "/provider-presets", None).await;
    assert_eq!(st, 200);
    let p = presets.as_array().unwrap().iter().find(|p| p["name"] == preset).unwrap_or_else(|| panic!("{preset}"));
    let mut q = p["quirks"].clone();
    for (k, v) in quirks.as_object().unwrap() {
        q[k] = v.clone();
    }
    let mut body = json!({ "name": name, "family": p["family"], "base_url": base_url, "quirks": q });
    if let Some(auth) = p.get("auth") {
        body["auth"] = auth.clone();
    }
    let id = gw.create("/providers", body).await.id;
    gw.create(&format!("/providers/{id}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
    id
}

async fn setup() -> World {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let base = mock.base_url();
    let rates = json!({ "currency": "USD", "input": 1.0, "output": 4.0, "cache_read": 0.1, "cache_write_5m": 1.0 });
    let models = [
        (
            "deepseek",
            "deepseek",
            format!("{base}/emulate/deepseek"),
            json!({}),
            "deepseek/flash",
            "deepseek-flash",
            "deepseek-vision",
        ),
        (
            "glm",
            "glm",
            format!("{base}/emulate/glm/api/paas/v4"),
            json!({ "request_fields": { "thinking": { "clear_thinking": false } } }),
            "glm/4.7",
            "glm-4.7",
            "glm-4",
        ),
        ("kimi", "kimi", format!("{base}/emulate/kimi/v1"), json!({}), "kimi/k3", "kimi-k3", "kimi-k3"),
        (
            "ollama",
            "ollama",
            format!("{base}/emulate/ollama/v1"),
            json!({}),
            "ollama/qwen",
            "qwen3:8b",
            "ollama-thinking",
        ),
    ];
    let mut glm = String::new();
    for (preset, name, url, quirks, model, upstream, capabilities) in models {
        let p = provider(&gw, preset, name, &url, quirks).await;
        if name == "glm" {
            glm = p.clone();
        }
        let id = gw.draft_model(&p, model, upstream, json!({ "capability_preset": capabilities })).await;
        let (st, v) = gw.api("PUT", &format!("/models/{id}/price"), Some(rates.clone())).await;
        assert_eq!(st, 201, "{v}");
        gw.activate(&id).await;
        gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    }
    // GLM-5 takes an effort, not the switch.
    let id = gw.draft_model(&glm, "glm/5.3", "glm-5.3", json!({ "free": true, "capability_preset": "glm-5" })).await;
    gw.activate(&id).await;
    gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    World { gw, mock, sink, fx }
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

    fn data(&self) -> Vec<Value> {
        self.text
            .lines()
            .filter_map(|l| l.strip_prefix("data: "))
            .filter(|d| *d != "[DONE]")
            .map(|d| serde_json::from_str(d).unwrap())
            .collect()
    }
}

impl World {
    async fn post(&self, path: &str, anthropic: bool, body: Value) -> Reply {
        let mut req = self.gw.http.post(format!("{}{path}", self.gw.data)).header("content-type", "application/json");
        req = if anthropic {
            req.header("x-api-key", &self.fx.key).header("anthropic-version", "2023-06-01")
        } else {
            req.bearer_auth(&self.fx.key)
        };
        let r = req.body(body.to_string()).send().await.expect("request");
        Reply { status: r.status().as_u16(), headers: r.headers().clone(), text: r.text().await.expect("body") }
    }

    fn sent(&self, n: usize) -> Value {
        self.mock.captured()[n].json().expect("the provider got JSON")
    }
}

fn lookup_tool_openai() -> Value {
    json!([{ "type": "function", "function": { "name": "lookup", "parameters": { "type": "object" } } }])
}

#[tokio::test]
async fn deepseek_gets_its_reasoning_back_on_a_translated_tool_loop() {
    let w = setup().await;
    let tools = json!([{ "name": "lookup", "input_schema": { "type": "object" } }]);
    let user = json!({ "role": "user", "content": "find it" });
    script(&w.mock, &["tool=lookup"]);
    let first = w
        .post(
            "/v1/messages",
            true,
            json!({ "model": "deepseek/flash", "max_tokens": 500, "tools": tools, "messages": [user] }),
        )
        .await;
    assert_eq!(first.status, 200, "{}", first.text);
    let answer = first.json();
    // DeepSeek thinks by default: its reasoning comes back as a thinking block.
    assert_eq!(answer["content"][0]["type"], "thinking", "{answer}");
    let call = answer["content"].as_array().unwrap().iter().find(|b| b["type"] == "tool_use").unwrap().clone();

    let result =
        json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": call["id"], "content": "42" }] });
    let second = w
        .post(
            "/v1/messages",
            true,
            json!({ "model": "deepseek/flash", "max_tokens": 500, "tools": tools,
                    "messages": [user, { "role": "assistant", "content": answer["content"] }, result] }),
        )
        .await;
    assert_eq!(second.status, 200, "the emulation refuses a tool loop without the reasoning: {}", second.text);
    let sent = w.sent(1);
    let assistant = sent["messages"].as_array().unwrap().iter().find(|m| m["role"] == "assistant").unwrap();
    assert_eq!(assistant["reasoning_content"], answer["content"][0]["thinking"]);
}

#[tokio::test]
async fn a_turn_without_reasoning_goes_to_deepseek_with_an_empty_one() {
    let w = setup().await;
    // A history another provider served: no reasoning on the assistant turn.
    let r = w
        .post(
            "/gw/v1/chat",
            false,
            json!({ "model": "deepseek/flash", "max_output_tokens": 200,
                    "tools": [{ "type": "function", "name": "lookup", "parameters": { "type": "object" } }],
                    "messages": [
                        { "role": "user", "content": "find it" },
                        { "role": "assistant", "content": [{ "type": "tool_call", "id": "c1", "name": "lookup", "arguments": "{}" }] },
                        { "role": "tool", "content": [{ "type": "tool_result", "tool_call_id": "c1", "content": "42" }] }
                    ] }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let sent = w.sent(0);
    assert_eq!(sent["messages"][1]["reasoning_content"], "");
}

#[tokio::test]
async fn the_same_dialect_tool_loop_keeps_the_callers_bytes() {
    let w = setup().await;
    script(&w.mock, &["tool=lookup"]);
    let user = json!({ "role": "user", "content": "find it" });
    let first = w
        .post(
            "/v1/chat/completions",
            false,
            json!({ "model": "deepseek/flash", "tools": lookup_tool_openai(), "messages": [user] }),
        )
        .await;
    assert_eq!(first.status, 200, "{}", first.text);
    let message = first.json()["choices"][0]["message"].clone();
    assert!(message["reasoning_content"].is_string(), "{message}");
    let call_id = message["tool_calls"][0]["id"].clone();
    let body = json!({ "model": "deepseek/flash", "tools": lookup_tool_openai(), "messages": [
        user, message, { "role": "tool", "tool_call_id": call_id, "content": "42" } ] });
    let second = w.post("/v1/chat/completions", false, body.clone()).await;
    assert_eq!(second.status, 200, "{}", second.text);
    assert_eq!(w.sent(1)["messages"], body["messages"], "the conversation is forwarded as sent");
}

#[tokio::test]
async fn a_used_up_quota_is_not_retried_and_a_rate_limit_is() {
    let w = setup().await;
    let body = json!({ "model": "glm/4.7", "messages": [{ "role": "user", "content": "hi" }] });
    script(&w.mock, &["status=429,code=1113"]);
    let r = w.post("/v1/chat/completions", false, body.clone()).await;
    assert_eq!(r.status, 402, "{}", r.text);
    assert_eq!(r.json()["error"]["type"], "insufficient_quota");
    assert!(r.json()["error"]["message"].as_str().unwrap().contains("1113"), "{}", r.text);
    assert_eq!(w.mock.captured().len(), 1, "a used-up balance is not asked again");

    w.mock.clear();
    script(&w.mock, &["fail=429x1"]);
    let r = w.post("/v1/chat/completions", false, body).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(w.mock.captured().len(), 2, "GLM's 1302 is a passing rate limit: retried");
}

#[tokio::test]
async fn a_used_up_quota_moves_the_call_to_the_next_model() {
    let w = setup().await;
    w.gw.template(&w.fx, "bot", json!({ "models": [{ "model": "glm/4.7" }, { "model": "openai/m", "priority": 1 }] }))
        .await;
    script(&w.mock, &["status=429,code=1113"]);
    let r = w
        .post(
            "/v1/chat/completions",
            false,
            json!({ "model": "@bot", "messages": [{ "role": "user", "content": "hi" }] }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!((r.header("x-gw-model-served"), r.header("x-gw-failovers")), (Some("openai/m"), Some("1")));
}

#[tokio::test]
async fn kimi_samplers_are_left_out() {
    let w = setup().await;
    let r = w
        .post(
            "/v1/chat/completions",
            false,
            json!({ "model": "kimi/k3", "temperature": 0.2, "top_p": 0.5, "messages": [{ "role": "user", "content": "hi" }] }),
        )
        .await;
    assert_eq!(r.status, 200, "the emulation refuses any other value: {}", r.text);
    assert_eq!(r.header("x-gw-dropped-params"), Some("temperature,top_p"));
    let sent = w.sent(0);
    assert!(sent.get("temperature").is_none() && sent.get("top_p").is_none(), "{sent}");
    // Usage with cache writes: counted apart from the input.
    script(&w.mock, &["cache_read=4,cache_write=6"]);
    let r = w
        .post(
            "/v1/chat/completions",
            false,
            json!({ "model": "kimi/k3", "messages": [{ "role": "user", "content": "hi again" }] }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let events = w.sink.wait_events(2).await;
    let u = &events.iter().find(|e| e.usage.cache_write_tokens > 0).expect("an event with cache writes").usage;
    assert_eq!((u.cache_read_tokens, u.cache_write_tokens), (4, 6));
}

#[tokio::test]
async fn glm_switches_thinking_and_keeps_its_own_members() {
    let w = setup().await;
    let hi = json!([{ "role": "user", "content": "hi" }]);
    let r = w
        .post("/v1/chat/completions", false, json!({ "model": "glm/4.7", "reasoning_effort": "none", "messages": hi }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(w.sent(0)["thinking"], json!({ "clear_thinking": false, "type": "disabled" }));
    assert!(w.sent(0).get("reasoning_effort").is_none());

    // The caller's own member wins over the provider's default.
    let r = w
        .post(
            "/v1/chat/completions",
            false,
            json!({ "model": "glm/4.7", "thinking": { "type": "enabled", "clear_thinking": true }, "messages": hi }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(w.sent(1)["thinking"], json!({ "type": "enabled", "clear_thinking": true }));

    // GLM-5 takes an effort.
    let r = w
        .post("/v1/chat/completions", false, json!({ "model": "glm/5.3", "reasoning_effort": "low", "messages": hi }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(w.sent(2)["reasoning_effort"], "low");
    assert_eq!(w.sent(2)["thinking"], json!({ "clear_thinking": false }));

    // The translated route adds the same members.
    let r = w.post("/v1/messages", true, json!({ "model": "glm/4.7", "max_tokens": 100, "messages": hi })).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(w.sent(3)["thinking"]["clear_thinking"], false);

    // Tool choice none is not GLM's: left out and said.
    let r = w
        .post(
            "/v1/chat/completions",
            false,
            json!({ "model": "glm/4.7", "tools": lookup_tool_openai(), "tool_choice": "none", "messages": hi }),
        )
        .await;
    assert_eq!((r.status, r.header("x-gw-dropped-params")), (200, Some("tool_choice")), "{}", r.text);
}

#[tokio::test]
async fn glm_streams_usage_unasked_and_the_cost_is_added() {
    let w = setup().await;
    let hi = json!([{ "role": "user", "content": "hi" }]);
    let r = w
        .post(
            "/v1/chat/completions",
            false,
            json!({ "model": "glm/4.7", "stream": true, "stream_options": { "include_usage": true }, "messages": hi }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(w.sent(0).get("stream_options").is_none(), "not in GLM's schema");
    let chunks = r.data();
    let last = chunks.iter().rev().find(|c| c.get("usage").is_some_and(Value::is_object)).expect("a chunk with usage");
    assert_eq!(last["choices"][0]["finish_reason"], "stop");
    assert!(last["usage"]["cost"]["amount"].is_number(), "{last}");

    let r = w.post("/v1/chat/completions", false, json!({ "model": "glm/4.7", "stream": true, "messages": hi })).await;
    assert!(r.data().iter().all(|c| c.get("usage").is_none()), "not asked for: left out");
}

#[tokio::test]
async fn ollama_reasoning_reaches_every_interface() {
    let w = setup().await;
    let hi = json!([{ "role": "user", "content": "hi" }]);
    let r = w
        .post("/gw/v1/chat", false, json!({ "model": "ollama/qwen", "reasoning": { "effort": "low" }, "messages": hi }))
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(w.sent(0)["reasoning_effort"], "low");
    let v = r.json();
    assert_eq!(v["content"][0]["type"], "reasoning", "Ollama's `reasoning` field is read: {v}");

    let r = w
        .post(
            "/v1/messages",
            true,
            json!({ "model": "ollama/qwen", "max_tokens": 200, "stream": true,
                                            "output_config": { "effort": "low" }, "messages": hi }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(r.text.contains("\"thinking_delta\""), "{}", r.text);
}
