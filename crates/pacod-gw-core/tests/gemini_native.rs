//! Managed mode on Gemini's own API (`managed_api: native`), end to end against the mock's
//! emulation of Gemini 3, which refuses a tool loop without its thought signatures.

mod common;

use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_testkit::mock::{Behavior, MockProvider};
use serde_json::{Value, json};

struct World {
    gw: TestGw,
    mock: MockProvider,
    sink: IngestSink,
    fx: Fixture,
    provider: String,
}

async fn setup() -> World {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let (_, presets) = gw.api("GET", "/provider-presets", None).await;
    let preset = presets.as_array().unwrap().iter().find(|p| p["name"] == "gemini").unwrap().clone();
    assert_eq!(preset["managed_api"], "native");
    let provider = gw
        .create(
            "/providers",
            json!({ "name": "gemini", "family": "gemini", "managed_api": "native",
                    "base_url": format!("{}/emulate/gemini/v1beta", mock.base_url()), "quirks": preset["quirks"] }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{provider}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
    let id = gw
        .draft_model(&provider, "gemini/flash", "gemini-3.8-flash", json!({ "capability_preset": "gemini-3-flash" }))
        .await;
    let (st, v) = gw
        .api(
            "PUT",
            &format!("/models/{id}/price"),
            Some(json!({ "currency": "USD", "input": 1.0, "output": 4.0, "cache_read": 0.1 })),
        )
        .await;
    assert_eq!(st, 201, "{v}");
    // With the probe: Gemini's own models.get, with Gemini's own key header.
    let (st, v) = gw.api("POST", &format!("/models/{id}/transition"), Some(json!({ "to": "active" }))).await;
    assert_eq!(st, 200, "{v}");
    gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    World { gw, mock, sink, fx, provider }
}

impl World {
    async fn post(&self, path: &str, body: Value) -> (u16, reqwest::header::HeaderMap, String) {
        let mut req = self.gw.http.post(format!("{}{path}", self.gw.data));
        req = if path == "/v1/messages" {
            req.header("x-api-key", &self.fx.key).header("anthropic-version", "2023-06-01")
        } else {
            req.bearer_auth(&self.fx.key)
        };
        let r = req.json(&body).send().await.unwrap();
        (r.status().as_u16(), r.headers().clone(), r.text().await.unwrap())
    }

    fn script(&self, entries: &[&str]) {
        self.mock.set_script(Behavior::parse_all(entries.iter().copied()).unwrap());
    }

    fn sent(&self) -> (String, Value) {
        let c = self.mock.captured().into_iter().rev().find(|c| c.method == "POST").unwrap();
        (c.path.clone(), c.json().unwrap())
    }
}

#[tokio::test]
async fn every_interface_reaches_geminis_own_api() {
    let w = setup().await;
    let probe = w.mock.captured().into_iter().find(|c| c.method == "GET").unwrap();
    assert_eq!(probe.path, "/emulate/gemini/v1beta/models/gemini-3.8-flash");
    assert_eq!(probe.header("x-goog-api-key"), Some(PROVIDER_SECRET));

    let hi = json!([{ "role": "user", "content": "hello" }]);
    let (st, _, text) = w.post("/v1/chat/completions", json!({ "model": "gemini/flash", "messages": hi })).await;
    assert_eq!(st, 200, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["choices"][0]["message"]["content"], "echo: hello");
    assert!(v["usage"]["cost"]["amount"].is_number(), "{v}");
    let (path, body) = w.sent();
    assert_eq!(path, "/emulate/gemini/v1beta/models/gemini-3.8-flash:generateContent");
    assert_eq!(body["contents"], json!([{ "role": "user", "parts": [{ "text": "hello" }] }]));
    let c = w.mock.captured().into_iter().rev().find(|c| c.method == "POST").unwrap();
    assert_eq!(c.header("x-goog-api-key"), Some(PROVIDER_SECRET));
    assert_eq!(c.header("authorization"), None);

    let (st, _, text) = w
        .post("/v1/messages", json!({ "model": "gemini/flash", "max_tokens": 200, "stream": true, "messages": hi }))
        .await;
    assert_eq!(st, 200, "{text}");
    assert!(text.contains("event: message_stop") && text.contains("\"text_delta\""), "{text}");
    assert_eq!(w.sent().0, "/emulate/gemini/v1beta/models/gemini-3.8-flash:streamGenerateContent?alt=sse");

    w.script(&["thinking"]);
    let (st, _, text) = w
        .post("/gw/v1/chat", json!({ "model": "gemini/flash", "reasoning": { "effort": "high" }, "messages": hi }))
        .await;
    assert_eq!(st, 200, "{text}");
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(v["content"][0]["type"], "reasoning", "{v}");
    assert_eq!(
        w.sent().1["generationConfig"]["thinkingConfig"],
        json!({ "thinkingLevel": "high", "includeThoughts": true })
    );
    let e = w
        .sink
        .wait_events(3)
        .await
        .into_iter()
        .find(|e| e.usage.reasoning_tokens > 0)
        .expect("an event with reasoning");
    assert!(e.usage.output_tokens > e.usage.reasoning_tokens, "{:?}", e.usage);
}

#[tokio::test]
async fn a_tool_loop_carries_its_signatures_through_every_interface() {
    let w = setup().await;
    let user = json!({ "role": "user", "content": "find it" });

    // OpenAI interface.
    let tools = json!([{ "type": "function", "function": { "name": "lookup", "parameters": { "type": "object" } } }]);
    w.script(&["tool=lookup"]);
    let (st, _, text) =
        w.post("/v1/chat/completions", json!({ "model": "gemini/flash", "tools": tools, "messages": [user] })).await;
    assert_eq!(st, 200, "{text}");
    let message = serde_json::from_str::<Value>(&text).unwrap()["choices"][0]["message"].clone();
    let raw = message["tool_calls"][0]["extra_content"]["google"]["thought_signature"].as_str().unwrap().to_owned();
    let call_id = message["tool_calls"][0]["id"].clone();
    let (st, _, text) = w
        .post(
            "/v1/chat/completions",
            json!({ "model": "gemini/flash", "tools": tools, "messages": [user, message, { "role": "tool", "tool_call_id": call_id, "content": "42" }] }),
        )
        .await;
    assert_eq!(st, 200, "the emulation refuses a call without its signature: {text}");
    let (_, body) = w.sent();
    assert_eq!(body["contents"][1]["parts"][0]["thoughtSignature"], raw.as_str());
    assert_eq!(body["contents"][2]["parts"][0]["functionResponse"]["name"], "lookup");

    // Anthropic interface.
    let tools = json!([{ "name": "lookup", "input_schema": { "type": "object" } }]);
    w.script(&["tool=lookup"]);
    let (st, _, text) = w
        .post("/v1/messages", json!({ "model": "gemini/flash", "max_tokens": 300, "tools": tools, "messages": [user] }))
        .await;
    assert_eq!(st, 200, "{text}");
    let content = serde_json::from_str::<Value>(&text).unwrap()["content"].clone();
    let use_id = content.as_array().unwrap().iter().find(|b| b["type"] == "tool_use").unwrap()["id"].clone();
    let (st, _, text) = w
        .post(
            "/v1/messages",
            json!({ "model": "gemini/flash", "max_tokens": 300, "tools": tools, "messages": [
                user, { "role": "assistant", "content": content },
                { "role": "user", "content": [{ "type": "tool_result", "tool_use_id": use_id, "content": "42" }] } ] }),
        )
        .await;
    assert_eq!(st, 200, "{text}");
}

#[tokio::test]
async fn geminis_own_tools_and_objects_are_governed() {
    let w = setup().await;
    let hi = json!([{ "role": "user", "content": "hello" }]);
    // Google Search, passed as Gemini writes it.
    let (st, _, text) = w
        .post(
            "/gw/v1/chat",
            json!({ "model": "gemini/flash", "messages": hi, "tools": [{ "provider": "gemini", "definition": { "googleSearch": {} } }] }),
        )
        .await;
    assert_eq!(st, 200, "{text}");
    assert_eq!(w.sent().1["tools"], json!([{ "googleSearch": {} }]));

    // A context cache on the shared credential is not this workspace's to use.
    let (st, headers, text) = w
        .post(
            "/gw/v1/chat",
            json!({ "model": "gemini/flash", "messages": hi, "provider_options": { "gemini": { "cachedContent": "cachedContents/x" } } }),
        )
        .await;
    assert_eq!(st, 403, "{text}");
    assert_eq!(headers["x-gw-error-type"], "own_credential_required");

    // A workspace that allows web search only: Google Maps is refused.
    w.gw.api(
        "PUT",
        &format!("/orgs/{}/workspaces/{}", w.fx.org, w.fx.ws),
        Some(json!({ "name": "prod", "tag_keys": ["purpose", "site"], "settings": { "provider_tools": ["web_search"] } })),
    )
    .await;
    let (st, _, _) = w
        .post(
            "/gw/v1/chat",
            json!({ "model": "gemini/flash", "messages": hi, "tools": [{ "provider": "gemini", "definition": { "googleMaps": {} } }] }),
        )
        .await;
    assert_eq!(st, 403);
}

#[tokio::test]
async fn discovery_reads_geminis_own_list() {
    let w = setup().await;
    let (st, v) = w.gw.api("POST", "/discovery/runs", None).await;
    assert_eq!(st, 202, "{v}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let (_, runs) = w.gw.api("GET", "/discovery/runs", None).await;
        if runs[0]["finished_at"].is_string() {
            assert_eq!(runs[0]["result"]["gemini"]["listed"], 2, "the embedding model is not a chat model: {runs}");
            break;
        }
        assert!(std::time::Instant::now() < deadline, "{runs}");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    let (_, open) = w.gw.api("GET", "/discovery/proposals", None).await;
    let lite = open
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider_id"] == w.provider.as_str() && p["upstream_id"] == "gemini-3.5-flash-lite")
        .expect("the model the repository lacks");
    assert_eq!(lite["details"]["context_window"], 1_048_576);
    assert_eq!(lite["details"]["reasons"], true, "Gemini's list says which models think");
}
