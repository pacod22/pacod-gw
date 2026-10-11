//! The Gemini interface of managed mode: Gemini's SDKs, in their Gemini API mode and their
//! Vertex AI mode, reach OpenAI-shaped, Anthropic, Gemini and Vertex AI models.

mod common;

use base64::Engine as _;
use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_testkit::google::TEST_SERVICE_ACCOUNT;
use pacod_gw_testkit::mock::{Behavior, MockProvider};
use serde_json::{Value, json};

struct World {
    gw: TestGw,
    mock: MockProvider,
    sink: IngestSink,
    fx: Fixture,
}

/// The fixture's Anthropic provider with Claude; an OpenAI provider with a chat model, a model
/// whose id holds a colon and an embedding model; Gemini's own API and Vertex AI with a chat
/// and an embedding model each; and a template on the OpenAI chat model.
async fn setup() -> World {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let token_url = format!("{}/emulate/google-oauth/token", mock.base_url());
    let db = common::fresh_database().await;
    let gw = TestGw::start_on(&db, Some(&sink), move |b| b.google_token_url = Some(token_url)).await;
    let fx = gw.fixture("anthropic", &mock.base_url()).await;
    let price = json!({ "currency": "USD", "input": 1.0, "output": 4.0 });
    gw.priced_model(
        &fx,
        "anthropic/claude",
        "claude-sonnet-5-5",
        json!({ "capability_preset": "anthropic-adaptive", "max_output_tokens": 16000 }),
        price.clone(),
    )
    .await;
    let provider = |body: Value| {
        let gw = &gw;
        async move { gw.create("/providers", body).await.id }
    };
    let openai =
        provider(json!({ "name": "openai", "family": "openai", "base_url": format!("{}/v1", mock.base_url()) })).await;
    let gemini = provider(json!({ "name": "gemini", "family": "gemini", "managed_api": "native",
                                  "base_url": format!("{}/emulate/gemini/v1beta", mock.base_url()) }))
    .await;
    let vertex = provider(json!({ "name": "vertex", "family": "gemini", "managed_api": "native", "platform": "vertex",
                                  "auth": "google_service_account",
                                  "base_url": format!("{}/emulate/vertex/v1/projects/p/locations/europe-west4/publishers/google", mock.base_url()) }))
    .await;
    gw.create(&format!("/providers/{openai}/credentials"), json!({ "secret": "sk-openai-side" })).await;
    gw.create(&format!("/providers/{gemini}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
    gw.create(&format!("/providers/{vertex}/credentials"), json!({ "secret": TEST_SERVICE_ACCOUNT })).await;
    let embedding = json!({ "kind": "embedding", "capability_preset": "embedding" });
    let flash = json!({ "capability_preset": "gemini-3-flash" });
    for (p, name, upstream, extra) in [
        (&openai, "openai/gpt", "gpt-x", json!({ "capability_preset": "openai-reasoning" })),
        (&openai, "openai/llama3:8b", "llama3:8b", json!({})),
        (&openai, "openai/embed", "text-embedding-3-small", embedding.clone()),
        (&gemini, "gemini/flash", "gemini-3.8-flash", flash.clone()),
        (&gemini, "gemini/embed", "gemini-embedding-001", embedding.clone()),
        (&vertex, "vertex/flash", "gemini-3.8-flash", flash.clone()),
        (&vertex, "vertex/embed", "text-embedding-005", embedding.clone()),
    ] {
        let id = gw.draft_model(p, name, upstream, extra).await;
        let (st, v) = gw.api("PUT", &format!("/models/{id}/price"), Some(price.clone())).await;
        assert_eq!(st, 201, "{v}");
        gw.activate(&id).await;
        gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    }
    gw.template(&fx, "thinker", json!({ "model": "openai/gpt", "max_output_tokens": 300 })).await;
    World { gw, mock, sink, fx }
}

struct Reply {
    status: u16,
    headers: reqwest::header::HeaderMap,
    text: String,
}

impl Reply {
    fn json(&self) -> Value {
        serde_json::from_str(&self.text).unwrap_or_else(|e| panic!("not JSON ({e}): {}", self.text))
    }

    fn content_type(&self) -> &str {
        self.headers.get("content-type").and_then(|v| v.to_str().ok()).unwrap_or_default()
    }

    /// The chunks of a stream of server-sent events.
    fn chunks(&self) -> Vec<Value> {
        self.text.lines().filter_map(|l| l.strip_prefix("data: ")).map(|d| serde_json::from_str(d).unwrap()).collect()
    }
}

fn hello() -> Value {
    json!({ "contents": [{ "role": "user", "parts": [{ "text": "hello" }] }] })
}

impl World {
    /// A request as Gemini's SDK sends it: the key in `x-goog-api-key`.
    async fn post(&self, path: &str, body: Value) -> Reply {
        self.send(
            self.gw.http.post(format!("{}{path}", self.gw.data)).header("x-goog-api-key", &self.fx.key).json(&body),
        )
        .await
    }

    async fn send(&self, req: reqwest::RequestBuilder) -> Reply {
        let r = req.send().await.unwrap();
        Reply { status: r.status().as_u16(), headers: r.headers().clone(), text: r.text().await.unwrap() }
    }

    fn script(&self, entries: &[&str]) {
        self.mock.set_script(Behavior::parse_all(entries.iter().copied()).unwrap());
    }

    /// The last body the provider received at a path ending in `suffix`.
    fn sent(&self, suffix: &str) -> Value {
        let c = self.mock.captured().into_iter().rev().find(|c| c.path.ends_with(suffix)).unwrap();
        c.json().unwrap()
    }
}

#[tokio::test]
async fn gemini_callers_reach_every_provider() {
    let w = setup().await;
    let body = json!({
        "systemInstruction": { "parts": [{ "text": "Be brief." }] },
        "contents": [{ "role": "user", "parts": [{ "text": "hello" }] }],
        "generationConfig": { "maxOutputTokens": 200, "temperature": 1 }
    });
    // An OpenAI-shaped model.
    let r = w.post("/v1beta/models/openai/gpt:generateContent", body.clone()).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let v = r.json();
    assert!(v["candidates"][0]["content"]["parts"][0]["text"].as_str().is_some_and(|t| !t.is_empty()), "{v}");
    assert_eq!(
        (v["candidates"][0]["finishReason"].as_str(), v["modelVersion"].as_str()),
        (Some("STOP"), Some("openai/gpt"))
    );
    assert!(v["usageMetadata"]["promptTokenCount"].as_u64().is_some_and(|n| n > 0));
    let sent = w.sent("/v1/chat/completions");
    assert_eq!(sent["messages"][0], json!({ "role": "system", "content": "Be brief." }));
    assert_eq!((sent["model"].as_str(), sent["max_completion_tokens"].as_u64()), (Some("gpt-x"), Some(200)));
    assert!(r.headers.get("x-gw-cost").is_some());
    // Claude.
    let r = w.post("/v1beta/models/anthropic/claude:generateContent", body.clone()).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let sent = w.sent("/v1/messages");
    assert_eq!((sent["system"][0]["text"].as_str(), sent["max_tokens"].as_u64()), (Some("Be brief."), Some(200)));
    // Gemini's own API and Vertex AI, through the neutral model.
    for model in ["gemini/flash", "vertex/flash"] {
        let r = w.post(&format!("/v1beta/models/{model}:generateContent"), body.clone()).await;
        assert_eq!(r.status, 200, "{model}: {}", r.text);
        assert_eq!(w.sent(":generateContent")["generationConfig"]["maxOutputTokens"], 200);
    }
    let events = w.sink.wait_events(4).await;
    for e in &events {
        assert_eq!((e.interface.as_str(), e.path.as_str()), ("gemini", "/v1beta/models/{model}:generateContent"));
        assert_eq!(e.usage_source, "provider", "{}", e.model_served);
    }
}

#[tokio::test]
async fn streams_come_as_events_or_as_one_array() {
    let w = setup().await;
    for model in ["openai/gpt", "anthropic/claude", "gemini/flash"] {
        let r = w.post(&format!("/v1beta/models/{model}:streamGenerateContent?alt=sse"), hello()).await;
        assert_eq!(r.status, 200, "{model}: {}", r.text);
        assert!(r.content_type().starts_with("text/event-stream"), "{model}");
        let chunks = r.chunks();
        let text: String =
            chunks.iter().filter_map(|c| c["candidates"][0]["content"]["parts"][0]["text"].as_str()).collect();
        assert!(!text.is_empty(), "{model}: {}", r.text);
        let last = chunks.last().unwrap();
        assert_eq!(last["candidates"][0]["finishReason"], "STOP", "{model}");
        assert!(last["usageMetadata"]["totalTokenCount"].as_u64().is_some_and(|n| n > 0), "{model}");
        // Without alt=sse: the same chunks as one JSON array.
        let r = w.post(&format!("/v1beta/models/{model}:streamGenerateContent"), hello()).await;
        assert_eq!(r.status, 200, "{model}: {}", r.text);
        assert!(r.content_type().starts_with("application/json"), "{model}");
        let array = r.json();
        assert!(array.as_array().is_some_and(|a| a.len() > 1), "{model}: {array}");
    }
    // A stream that breaks ends with Google's error, and the array is still whole.
    w.script(&["reset_after=2,chunks=6"]);
    let r = w.post("/v1beta/models/openai/gpt:streamGenerateContent", hello()).await;
    let array = r.json();
    assert!(array.as_array().unwrap().last().unwrap().get("error").is_some(), "{array}");
}

#[tokio::test]
async fn a_tool_loop_through_claude_keeps_its_thinking_and_signature() {
    let w = setup().await;
    let tools = json!([{ "functionDeclarations": [{ "name": "lookup", "description": "Looks it up.",
        "parameters": { "type": "OBJECT", "properties": { "q": { "type": "STRING" } }, "required": ["q"] } }] }]);
    let user = json!({ "role": "user", "parts": [{ "text": "find it" }] });
    w.script(&["tool=lookup,thinking"]);
    let r = w
        .post(
            "/v1beta/models/anthropic/claude:generateContent",
            json!({ "contents": [user], "tools": tools, "generationConfig": { "thinkingConfig": { "includeThoughts": true } } }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let sent = w.sent("/v1/messages");
    assert_eq!(sent["tools"][0]["input_schema"]["properties"]["q"]["type"], "string", "Gemini's schema as JSON Schema");
    let model = r.json()["candidates"][0]["content"].clone();
    let parts = model["parts"].as_array().unwrap();
    let thought = parts.iter().find(|p| p["thought"] == true).unwrap();
    let wrapped = thought["thoughtSignature"].as_str().unwrap();
    let raw = base64::engine::general_purpose::STANDARD.decode(wrapped).unwrap();
    assert!(raw.starts_with(b"\0gw1\0"), "Claude's signature reaches the caller wrapped");
    let call = parts.iter().find_map(|p| p.get("functionCall")).unwrap();
    assert!(call["id"].as_str().is_some_and(|i| i.starts_with("toolu_")));
    // The caller answers without the call's id, as Gemini's SDK may.
    let answer = json!({ "role": "user", "parts": [{ "functionResponse": { "name": "lookup", "response": { "output": "42" } } }] });
    let r = w
        .post(
            "/v1beta/models/anthropic/claude:generateContent",
            json!({ "contents": [user, model, answer], "tools": tools }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let sent = w.sent("/v1/messages");
    let assistant = &sent["messages"][1]["content"];
    assert_eq!(assistant[0]["type"], "thinking");
    assert_eq!(assistant[0]["signature"].as_str().unwrap().as_bytes(), &raw[5..], "the signature Claude made");
    let tool_use = assistant.as_array().unwrap().iter().find(|b| b["type"] == "tool_use").unwrap();
    assert_eq!(sent["messages"][2]["content"][0]["tool_use_id"], tool_use["id"], "paired by name and order");
    assert_eq!(sent["messages"][2]["content"][0]["content"][0]["text"], "42");
}

#[tokio::test]
async fn paths_name_models_with_slashes_colons_and_templates() {
    let w = setup().await;
    let r = w.post("/v1beta/models/openai/llama3:8b:generateContent", hello()).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(w.sent("/v1/chat/completions")["model"], "llama3:8b");
    let r = w.post("/v1beta/models/@thinker:generateContent", hello()).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(w.sent("/v1/chat/completions")["max_completion_tokens"], 300, "the template's limit");
    let r = w.post("/v1beta/models/openai%2Fgpt:generateContent", hello()).await;
    assert_eq!(r.status, 200, "{}", r.text);
    // An SDK set to Gemini's v1.
    let r = w.post("/v1/models/openai/gpt:generateContent", hello()).await;
    assert_eq!(r.status, 200, "{}", r.text);
}

#[tokio::test]
async fn vertex_mode_paths_ignore_the_callers_project() {
    let w = setup().await;
    let regional = |model: &str| {
        format!("/v1beta1/projects/someone-else/locations/us-central1/publishers/{model}:generateContent")
    };
    // The SDK writes `vertex/flash` as publisher `vertex`, model `flash`; the key comes as a Bearer token.
    let r = w
        .send(
            w.gw.http
                .post(format!("{}{}", w.gw.data, regional("vertex/models/flash")))
                .bearer_auth(&w.fx.key)
                .header("x-goog-user-project", "someone-else")
                .json(&hello()),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let reached = w.mock.captured().into_iter().rev().find(|c| c.path.ends_with(":generateContent")).unwrap();
    assert!(reached.path.contains("/projects/p/locations/europe-west4/"), "{}", reached.path);
    // Express mode, and a model of another provider.
    let r = w.post("/v1/publishers/openai/models/gpt:generateContent", hello()).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let r = w.post(&regional("anthropic/models/claude").replace("v1beta1", "v1"), hello()).await;
    assert_eq!(r.status, 200, "{}", r.text);
    for c in w.mock.captured() {
        let all = format!("{} {:?} {}", c.path, c.headers, String::from_utf8_lossy(&c.body));
        assert!(!all.contains("someone-else"), "the caller's project reached the provider: {all}");
    }
    // Vertex AI's other resources are not served here.
    let r = w.post("/v1beta1/projects/x/locations/y/endpoints/123:predict", hello()).await;
    assert_eq!(r.status, 404, "{}", r.text);
}

#[tokio::test]
async fn a_key_in_the_query_is_taken_out_and_never_recorded() {
    let w = setup().await;
    let r = w
        .send(
            w.gw.http
                .post(format!("{}/v1beta/models/openai/gpt:generateContent?key={}", w.gw.data, w.fx.key))
                .json(&hello()),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    let e = w.sink.wait_events(1).await.remove(0);
    assert!(!format!("{e:?}").contains(&w.fx.key));
    for c in w.mock.captured() {
        let all = format!("{} {:?} {}", c.path, c.headers, String::from_utf8_lossy(&c.body));
        assert!(!all.contains(&w.fx.key), "the gateway key reached the provider");
    }
}

#[tokio::test]
async fn errors_come_in_googles_shape() {
    let w = setup().await;
    let error = |r: &Reply| (r.status, r.json()["error"]["status"].as_str().unwrap_or_default().to_owned());
    let r = w.post("/v1beta/models/openai/nothing:generateContent", hello()).await;
    assert_eq!(error(&r), (404, "NOT_FOUND".into()), "{}", r.text);
    let r = w.post("/v1beta/models/openai/gpt:generateContent", json!({ "generationConfig": {} })).await;
    assert_eq!(error(&r), (400, "INVALID_ARGUMENT".into()), "{}", r.text);
    assert!(r.text.contains("contents"));
    let r = w
        .send(
            w.gw.http
                .post(format!("{}/v1beta/models/openai/gpt:generateContent", w.gw.data))
                .header("x-goog-api-key", "pgw_wrong_key")
                .json(&hello()),
        )
        .await;
    assert_eq!(error(&r), (401, "UNAUTHENTICATED".into()));
    let r = w.post("/v1beta/models/openai/gpt:computeTokens", hello()).await;
    assert_eq!(error(&r), (404, "NOT_FOUND".into()));
    let r = w.post("/v1beta/files", json!({})).await;
    assert_eq!(error(&r), (404, "NOT_FOUND".into()));
    assert!(r.text.contains("proxy mode"), "{}", r.text);
    // An embedding model does not chat; a chat model does not embed.
    let r = w.post("/v1beta/models/openai/embed:generateContent", hello()).await;
    assert_eq!(error(&r), (400, "INVALID_ARGUMENT".into()), "{}", r.text);
}

#[tokio::test]
async fn gemini_callers_count_tokens_and_embed() {
    let w = setup().await;
    // Counting on Claude: Anthropic's endpoint, with only the members it takes.
    let r = w
        .post(
            "/v1beta/models/anthropic/claude:countTokens",
            json!({ "contents": [{ "role": "user", "parts": [{ "text": "How many?" }] }], "generationConfig": { "maxOutputTokens": 9 } }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(r.json()["totalTokens"].as_u64().is_some_and(|n| n > 0));
    let sent = w.sent("/v1/messages/count_tokens");
    assert!(sent.get("max_tokens").is_none() && sent.get("messages").is_some(), "{sent}");
    // On Gemini, in the form that wraps the whole request.
    let wrapped =
        json!({ "generateContentRequest": { "model": "models/gemini/flash", "contents": hello()["contents"] } });
    let r = w.post("/v1beta/models/gemini/flash:countTokens", wrapped).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert!(r.json()["totalTokens"].as_u64().is_some());
    assert_eq!(w.sent(":countTokens")["generateContentRequest"]["model"], "models/gemini-3.8-flash");
    let other = json!({ "generateContentRequest": { "model": "models/gemini/pro", "contents": hello()["contents"] } });
    assert_eq!(w.post("/v1beta/models/gemini/flash:countTokens", other).await.status, 400);
    let r = w.post("/v1beta/models/openai/gpt:countTokens", hello()).await;
    assert_eq!(r.status, 400, "{}", r.text);
    // Embeddings from an OpenAI-shaped model, in Gemini's shape.
    let one = json!({ "content": { "parts": [{ "text": "The quick brown fox." }] }, "outputDimensionality": 8 });
    let r = w.post("/v1beta/models/openai/embed:embedContent", one).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.json()["embedding"]["values"].as_array().unwrap().len(), 8);
    let sent = w.sent("/v1/embeddings");
    assert_eq!((sent["encoding_format"].as_str(), sent["dimensions"].as_u64()), (Some("float"), Some(8)));
    // A batch on Gemini's own API; every request names the path's model.
    let batch = |model: &str| {
        json!({ "requests": [
            { "model": format!("models/{model}"), "content": { "parts": [{ "text": "a" }] }, "taskType": "RETRIEVAL_QUERY" },
            { "model": format!("models/{model}"), "content": { "parts": [{ "text": "b" }] }, "taskType": "RETRIEVAL_DOCUMENT" }
        ] })
    };
    let r = w.post("/v1beta/models/gemini/embed:batchEmbedContents", batch("gemini/embed")).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.json()["embeddings"].as_array().unwrap().len(), 2);
    let sent = w.sent(":batchEmbedContents");
    assert_eq!(sent["requests"][1]["taskType"], "RETRIEVAL_DOCUMENT", "each input keeps its own options");
    assert_eq!(sent["requests"][0]["model"], "models/gemini-embedding-001");
    assert_eq!(w.post("/v1beta/models/gemini/embed:batchEmbedContents", batch("gemini/other")).await.status, 400);
    // Vertex AI's predict, from the SDK's Vertex mode.
    let predict = json!({ "instances": [{ "content": "a b c" }, { "content": "d e" }] });
    let r = w.post("/v1beta1/projects/x/locations/y/publishers/vertex/models/embed:predict", predict).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let v = r.json();
    assert_eq!(v["predictions"].as_array().unwrap().len(), 2);
    assert!(v["predictions"][0]["embeddings"]["statistics"]["token_count"].as_u64().is_some(), "{v}");
}

#[tokio::test]
async fn the_model_list_comes_in_geminis_shape() {
    let w = setup().await;
    let list = |path: &str| w.gw.http.get(format!("{}{path}", w.gw.data)).header("x-goog-api-key", &w.fx.key).send();
    let v: Value = list("/v1beta/models").await.unwrap().json().await.unwrap();
    let models = v["models"].as_array().unwrap();
    let find = |name: &str| models.iter().find(|m| m["name"] == name).unwrap_or_else(|| panic!("{name}: {v}"));
    assert_eq!(find("models/openai/gpt")["supportedGenerationMethods"][0], "generateContent");
    assert_eq!(find("models/gemini/embed")["supportedGenerationMethods"][0], "embedContent");
    assert!(find("models/@thinker").get("name").is_some());
    let one = list("/v1beta/models/openai/llama3:8b").await.unwrap();
    assert_eq!(one.status(), 200);
    assert_eq!(one.json::<Value>().await.unwrap()["name"], "models/openai/llama3:8b");
    let missing = list("/v1beta/models/openai/nothing").await.unwrap();
    assert_eq!(missing.status(), 404);
}

impl World {
    /// A body exactly as written, for checks on the bytes the provider receives.
    async fn post_raw(&self, path: &str, body: String) -> Reply {
        self.send(
            self.gw
                .http
                .post(format!("{}{path}", self.gw.data))
                .header("x-goog-api-key", &self.fx.key)
                .header("content-type", "application/json")
                .body(body),
        )
        .await
    }

    fn sent_raw(&self, suffix: &str) -> String {
        let c = self.mock.captured().into_iter().rev().find(|c| c.path.contains(suffix)).unwrap();
        String::from_utf8(c.body.to_vec()).unwrap()
    }
}

#[tokio::test]
async fn gemini_models_receive_the_callers_own_bytes() {
    let w = setup().await;
    w.gw.template(&w.fx, "short", json!({ "model": "gemini/flash", "max_output_tokens": 300 })).await;
    // Unusual spacing and both spellings: the contents keep their bytes, the configuration is governed.
    let contents = r#"[ {"role":"user",  "parts":[{"text":"hello"} ]} ]"#;
    let body = format!(
        r#"{{"contents": {contents}, "safetySettings":[{{"category":"HARM_CATEGORY_HARASSMENT","threshold":"BLOCK_NONE"}}], "tools":[{{"googleSearch":{{}}}}], "generation_config":{{"max_output_tokens":99999,"thinking_config":{{"thinking_level":"HIGH","include_thoughts":true}}}}, "labels":{{"team":"a"}}, "gateway":{{"tags":{{"purpose":"x"}}}}}}"#
    );
    let r = w.post_raw("/v1beta/models/@short:generateContent", body).await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(r.json()["modelVersion"], "gemini-3.8-flash", "Gemini's own answer, as it wrote it");
    let sent = w.sent_raw(":generateContent");
    assert!(sent.contains(&format!(r#""contents": {contents}"#)), "the caller's bytes: {sent}");
    let v: Value = serde_json::from_str(&sent).unwrap();
    assert_eq!(v["generationConfig"]["maxOutputTokens"], 300, "the template's limit: {sent}");
    // One spelling; the level as the caller wrote it (Gemini reads either case).
    assert_eq!(v["generationConfig"]["thinkingConfig"], json!({ "thinkingLevel": "HIGH", "includeThoughts": true }));
    for gone in ["generation_config", "labels", "gateway"] {
        assert!(v.get(gone).is_none(), "{gone}: {sent}");
    }
    assert!(v.get("safetySettings").is_some() && v["tools"][0].get("googleSearch").is_some());
    let dropped = r.headers.get("x-gw-dropped-params").and_then(|v| v.to_str().ok()).unwrap_or_default();
    assert!(dropped.contains("labels"), "{dropped}");
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.template.as_str(), e.tags.get("purpose").map(String::as_str)), ("short", Some("x")));
}

#[tokio::test]
async fn signatures_keep_to_their_origin_through_the_gemini_interface() {
    let w = setup().await;
    let tools = json!([{ "functionDeclarations": [{ "name": "lookup", "parameters": { "type": "object" } }] }]);
    let user = json!({ "role": "user", "parts": [{ "text": "find it" }] });
    let loop_on = |model: &'static str| {
        let (w, tools, user) = (&w, tools.clone(), user.clone());
        async move {
            w.script(&["tool=lookup"]);
            let path = format!("/v1beta/models/{model}:generateContent");
            let r = w.post(&path, json!({ "contents": [user], "tools": tools })).await;
            assert_eq!(r.status, 200, "{model}: {}", r.text);
            let turn = r.json()["candidates"][0]["content"].clone();
            let answer = json!({ "role": "user", "parts": [{ "functionResponse": { "name": "lookup", "response": { "output": "42" } } }] });
            // The emulation refuses a call of the current turn without its signature.
            let r = w.post(&path, json!({ "contents": [user, turn, answer], "tools": tools })).await;
            assert_eq!(r.status, 200, "{model}: {}", r.text);
            turn
        }
    };
    // Gemini's API, the caller's own bytes: its signature as Gemini wrote it.
    let studio = loop_on("gemini/flash").await;
    let raw = studio["parts"][0]["thoughtSignature"].as_str().unwrap().to_owned();
    assert!(!base64::engine::general_purpose::STANDARD.decode(&raw).unwrap().starts_with(b"\0gw1\0"));
    assert!(w.sent_raw(":generateContent").contains(&raw));
    // Vertex AI, through the neutral model: its signature labelled and wrapped.
    // The first call to Vertex AI mints its token at the mock; the script is for the call after.
    assert_eq!(w.post("/v1beta/models/vertex/flash:generateContent", hello()).await.status, 200);
    let vertex = loop_on("vertex/flash").await;
    let wrapped = vertex["parts"][0]["thoughtSignature"].as_str().unwrap().to_owned();
    let decoded = base64::engine::general_purpose::STANDARD.decode(&wrapped).unwrap();
    assert!(decoded.starts_with(b"\0gw1\0gw1.vertex."), "{}", String::from_utf8_lossy(&decoded));
    // A Vertex turn given to Gemini's API: not its signature, so Google's placeholder goes.
    let answer = json!({ "role": "user", "parts": [{ "functionResponse": { "name": "lookup", "response": { "output": "42" } } }] });
    let r = w
        .post(
            "/v1beta/models/gemini/flash:generateContent",
            json!({ "contents": [user, vertex, answer], "tools": tools }),
        )
        .await;
    assert_eq!(r.status, 200, "{}", r.text);
    assert_eq!(
        w.sent(":generateContent")["contents"][1]["parts"][0]["thoughtSignature"],
        "skip_thought_signature_validator"
    );
}

#[tokio::test]
async fn gemini_streams_pass_or_become_one_array() {
    let w = setup().await;
    let r = w.post("/v1beta/models/gemini/flash:streamGenerateContent?alt=sse", hello()).await;
    assert_eq!(r.status, 200, "{}", r.text);
    let chunks = r.chunks();
    assert!(chunks.iter().all(|c| c["modelVersion"] == "gemini-3.8-flash"), "Gemini's own events: {}", r.text);
    assert!(w.mock.captured().last().unwrap().path.ends_with(":streamGenerateContent?alt=sse"));
    let array = w.post("/v1beta/models/gemini/flash:streamGenerateContent", hello()).await.json();
    assert_eq!(array.as_array().unwrap(), &chunks, "the same events as one array");
    let e = w.sink.wait_events(2).await;
    assert!(e.iter().all(|e| e.usage_source == "provider" && e.usage.output_tokens > 0), "{e:?}");
    // A stream that breaks: Google's error ends it, in either framing.
    w.script(&["reset_after=2,chunks=6"]);
    let r = w.post("/v1beta/models/gemini/flash:streamGenerateContent", hello()).await;
    assert_eq!(r.json().as_array().unwrap().last().unwrap()["error"]["code"], 502, "{}", r.text);
    w.script(&["reset_after=2,chunks=6"]);
    let r = w.post("/v1beta/models/gemini/flash:streamGenerateContent?alt=sse", hello()).await;
    assert!(r.chunks().last().unwrap().get("error").is_some(), "{}", r.text);
}
