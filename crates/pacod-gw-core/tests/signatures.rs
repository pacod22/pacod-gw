//! Reasoning that only its provider can take back (concept section 8.3): Gemini 3's thought
//! signatures through every interface, and conversations that move between providers, where
//! Claude must never get a thinking block it did not sign.

mod common;

use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_testkit::mock::{Behavior, MockProvider};
use serde_json::{Value, json};

struct World {
    gw: TestGw,
    mock: MockProvider,
    _sink: IngestSink,
    fx: Fixture,
}

async fn from_preset(gw: &TestGw, preset: &str, name: &str, base_url: &str) -> String {
    let (_, presets) = gw.api("GET", "/provider-presets", None).await;
    let p = presets.as_array().unwrap().iter().find(|p| p["name"] == preset).unwrap().clone();
    let id = gw
        .create(
            "/providers",
            json!({ "name": name, "family": p["family"], "base_url": base_url, "quirks": p["quirks"] }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{id}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
    id
}

/// Claude (`anthropic/claude`), Gemini 3 at the mock's emulation of its compatible endpoint
/// (`gemini/flash`), and DeepSeek (`deepseek/flash`).
async fn setup() -> World {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("anthropic", &mock.base_url()).await;
    gw.model(
        &fx,
        "anthropic/claude",
        "claude-test",
        json!({ "free": true, "capability_preset": "anthropic-adaptive" }),
    )
    .await;
    let base = mock.base_url();
    for (preset, name, url, model, upstream, caps) in [
        (
            "gemini-openai",
            "gemini",
            format!("{base}/emulate/gemini-openai/v1beta/openai"),
            "gemini/flash",
            "gemini-3.8-flash",
            "gemini-3-flash",
        ),
        ("deepseek", "deepseek", format!("{base}/emulate/deepseek"), "deepseek/flash", "deepseek-flash", "deepseek"),
    ] {
        let p = from_preset(&gw, preset, name, &url).await;
        let id = gw.draft_model(&p, model, upstream, json!({ "free": true, "capability_preset": caps })).await;
        // With the probe: it must reach the compatible endpoint with a Bearer key.
        let (st, v) = gw.api("POST", &format!("/models/{id}/transition"), Some(json!({ "to": "active" }))).await;
        assert_eq!(st, 200, "{preset}: {v}");
        gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    }
    World { gw, mock, _sink: sink, fx }
}

impl World {
    async fn post(&self, path: &str, body: Value) -> (u16, Value) {
        let mut req = self.gw.http.post(format!("{}{path}", self.gw.data));
        req = if path == "/v1/messages" {
            req.header("x-api-key", &self.fx.key).header("anthropic-version", "2023-06-01")
        } else {
            req.bearer_auth(&self.fx.key)
        };
        let r = req.json(&body).send().await.unwrap();
        let status = r.status().as_u16();
        let text = r.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    fn script(&self, entries: &[&str]) {
        self.mock.set_script(Behavior::parse_all(entries.iter().copied()).unwrap());
    }

    fn last_sent(&self) -> Value {
        self.mock.captured().last().unwrap().json().unwrap()
    }
}

#[tokio::test]
async fn the_probe_reaches_geminis_compatible_endpoint_with_a_bearer_key() {
    let w = setup().await;
    let probe = w.mock.captured().into_iter().find(|c| c.path.contains("/emulate/gemini-openai/")).unwrap();
    assert_eq!(probe.path, "/emulate/gemini-openai/v1beta/openai/models/gemini-3.8-flash");
    assert_eq!(probe.header("authorization"), Some(format!("Bearer {PROVIDER_SECRET}").as_str()));
}

#[tokio::test]
async fn a_gemini_tool_loop_works_through_every_interface() {
    let w = setup().await;
    let tools_openai =
        json!([{ "type": "function", "function": { "name": "lookup", "parameters": { "type": "object" } } }]);
    let user = json!({ "role": "user", "content": "find it" });

    // OpenAI interface: Gemini's own shape passes both ways.
    w.script(&["tool=lookup"]);
    let (st, first) = w
        .post("/v1/chat/completions", json!({ "model": "gemini/flash", "tools": tools_openai, "messages": [user] }))
        .await;
    assert_eq!(st, 200, "{first}");
    let message = first["choices"][0]["message"].clone();
    assert!(message["tool_calls"][0]["extra_content"]["google"]["thought_signature"].is_string(), "{message}");
    let call_id = message["tool_calls"][0]["id"].clone();
    let (st, v) = w
        .post(
            "/v1/chat/completions",
            json!({ "model": "gemini/flash", "tools": tools_openai, "messages": [
                user, message, { "role": "tool", "tool_call_id": call_id, "content": "42" } ] }),
        )
        .await;
    assert_eq!(st, 200, "the emulation refuses a call without its signature: {v}");

    // Anthropic interface: the signature rides in a thinking block and comes back.
    let tools = json!([{ "name": "lookup", "input_schema": { "type": "object" } }]);
    w.script(&["tool=lookup"]);
    let (st, first) = w
        .post("/v1/messages", json!({ "model": "gemini/flash", "max_tokens": 300, "tools": tools, "messages": [user] }))
        .await;
    assert_eq!(st, 200, "{first}");
    let content = first["content"].clone();
    assert!(content[0]["signature"].as_str().unwrap().starts_with("gw1.gemini."), "{content}");
    let use_id = content.as_array().unwrap().iter().find(|b| b["type"] == "tool_use").unwrap()["id"].clone();
    let result =
        json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": use_id, "content": "42" }] });
    let (st, v) = w
        .post(
            "/v1/messages",
            json!({ "model": "gemini/flash", "max_tokens": 300, "tools": tools,
                    "messages": [user, { "role": "assistant", "content": content }, result] }),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let sent = w.last_sent();
    let assistant = sent["messages"].as_array().unwrap().iter().find(|m| m["role"] == "assistant").unwrap().clone();
    let raw = assistant["tool_calls"][0]["extra_content"]["google"]["thought_signature"].as_str().unwrap();
    assert!(!raw.starts_with("gw1."), "Gemini gets its own signature back");

    // Native interface, with the signature lost: Google's placeholder, and a warning.
    w.script(&["tool=lookup"]);
    let (st, v) = w
        .post(
            "/gw/v1/chat",
            json!({ "model": "gemini/flash",
                    "tools": [{ "name": "lookup", "parameters": { "type": "object" } }],
                    "messages": [
                        user,
                        { "role": "assistant", "content": [{ "type": "tool_call", "id": "c1", "name": "lookup", "arguments": "{}" }] },
                        { "role": "tool", "content": [{ "type": "tool_result", "tool_call_id": "c1", "content": "42" }] }
                    ] }),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let sent = w.last_sent();
    assert_eq!(
        sent["messages"][1]["tool_calls"][0]["extra_content"]["google"]["thought_signature"],
        "skip_thought_signature_validator"
    );
    let warnings = v["gateway"]["warnings"].to_string();
    assert!(warnings.contains("thought signature"), "{v}");
}

#[tokio::test]
async fn claude_never_gets_a_thinking_block_another_provider_made() {
    let w = setup().await;
    let user = json!({ "role": "user", "content": "find it" });
    let tools = json!([{ "name": "lookup", "input_schema": { "type": "object" } }]);
    for model in ["gemini/flash", "deepseek/flash"] {
        // Turn 1 at another provider, through Anthropic's interface.
        w.script(&["tool=lookup"]);
        let (st, first) = w
            .post("/v1/messages", json!({ "model": model, "max_tokens": 300, "tools": tools, "messages": [user] }))
            .await;
        assert_eq!(st, 200, "{model}: {first}");
        let content = first["content"].clone();
        assert_eq!(content[0]["type"], "thinking", "{model}: {content}");
        let use_id = content.as_array().unwrap().iter().find(|b| b["type"] == "tool_use").unwrap()["id"].clone();
        let result =
            json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": use_id, "content": "42" }] });

        // Turn 2 at Claude: the same dialect, so the history goes as sent, but for the block
        // Anthropic would refuse.
        let r =
            w.gw.http
                .post(format!("{}/v1/messages", w.gw.data))
                .header("x-api-key", &w.fx.key)
                .header("anthropic-version", "2023-06-01")
                .json(&json!({ "model": "anthropic/claude", "max_tokens": 300, "tools": tools,
                           "messages": [user, { "role": "assistant", "content": content }, result] }))
                .send()
                .await
                .unwrap();
        assert_eq!(r.status(), 200, "{model}");
        assert_eq!(r.headers().get("x-gw-dropped-params").and_then(|v| v.to_str().ok()), Some("thinking"), "{model}");
        let sent = w.last_sent();
        let text = sent["messages"].to_string();
        assert!(!text.contains("\"thinking\"") && !text.contains("gw1."), "{model}: {text}");
        assert!(text.contains("tool_use"), "the rest of the turn stays: {text}");
    }

    // A Claude conversation of its own keeps its bytes.
    let body = r#"{"model":"anthropic/claude","max_tokens":300,"messages":[{"role":"user","content":"q"},{"role":"assistant","content":[{"type":"thinking","thinking":"t","signature":"c2ln"},{"type":"text","text":"a"}]},{"role":"user","content":"more"}]}"#;
    w.mock.clear();
    let r =
        w.gw.http
            .post(format!("{}/v1/messages", w.gw.data))
            .header("x-api-key", &w.fx.key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 200);
    let sent = String::from_utf8(w.mock.captured()[0].body.to_vec()).unwrap();
    assert!(sent.contains(r#"{"type":"thinking","thinking":"t","signature":"c2ln"}"#), "{sent}");
}
