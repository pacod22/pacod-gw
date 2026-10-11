//! Provider emulations under `/emulate/<provider>/…`.

use pacod_gw_testkit::mock::{MockProvider, thinking_text};
use serde_json::{Value, json};

use crate::support::{REPLY, USER, get, post, sse_events};

fn chat(model: &str, stream: bool) -> Value {
    json!({ "model": model, "stream": stream, "messages": [{ "role": "user", "content": USER }] })
}

fn tools() -> Value {
    json!([{ "type": "function", "function": { "name": "lookup", "parameters": { "type": "object" } } }])
}

async fn json_of(mock: &MockProvider, path: &str, x_mock: &str, body: &Value) -> (u16, Value) {
    let response = post(mock, path).header("x-mock", x_mock).json(body).send().await.unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap())
}

/// The raw stream text and its JSON payloads (without `[DONE]`).
async fn stream_of(mock: &MockProvider, path: &str, x_mock: &str, body: &Value) -> (String, Vec<Value>) {
    let response = post(mock, path).header("x-mock", x_mock).json(body).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let text = response.text().await.unwrap();
    let chunks =
        sse_events(&text).iter().filter(|e| e.data != "[DONE]" && !e.data.is_empty()).map(|e| e.json()).collect();
    (text, chunks)
}

const DEEPSEEK: &str = "/emulate/deepseek/chat/completions";
const GLM: &str = "/emulate/glm/api/paas/v4/chat/completions";
const KIMI: &str = "/emulate/kimi/v1/chat/completions";
const GEMINI_OPENAI: &str = "/emulate/gemini-openai/v1beta/openai/chat/completions";
const OLLAMA: &str = "/emulate/ollama/v1/chat/completions";

#[tokio::test]
async fn deepseek_thinks_by_default_and_reports_cache_hits() {
    let mock = MockProvider::spawn().await;
    let (status, body) = json_of(&mock, DEEPSEEK, "cache_read=3", &chat("deepseek-flash", false)).await;
    assert_eq!(status, 200);
    let message = &body["choices"][0]["message"];
    assert_eq!(message["reasoning_content"], thinking_text(USER));
    assert_eq!(message["content"], REPLY);
    let usage = &body["usage"];
    assert_eq!(usage["prompt_cache_hit_tokens"], 3);
    assert_eq!(usage["prompt_tokens_details"]["cached_tokens"], 3);
    assert_eq!(
        usage["prompt_tokens"].as_u64().unwrap(),
        usage["prompt_cache_hit_tokens"].as_u64().unwrap() + usage["prompt_cache_miss_tokens"].as_u64().unwrap()
    );

    let mut off = chat("deepseek-flash", false);
    off["thinking"] = json!({ "type": "disabled" });
    let (_, body) = json_of(&mock, DEEPSEEK, "", &off).await;
    assert!(body["choices"][0]["message"].get("reasoning_content").is_none());
}

#[tokio::test]
async fn deepseek_streams_usage_on_the_finish_chunk_after_keep_alives() {
    let mock = MockProvider::spawn().await;
    let mut body = chat("deepseek-flash", true);
    body["stream_options"] = json!({ "include_usage": true });
    let (text, chunks) = stream_of(&mock, DEEPSEEK, "", &body).await;
    assert!(text.starts_with(": keep-alive\n\n"), "{text}");
    let last = chunks.last().unwrap();
    assert_eq!(last["choices"][0]["finish_reason"], "stop");
    assert!(last["usage"]["prompt_cache_miss_tokens"].is_u64(), "{last}");
    assert!(chunks[..chunks.len() - 1].iter().all(|c| c["usage"].is_null()));
}

#[tokio::test]
async fn deepseek_wants_the_reasoning_back_when_tools_are_present() {
    let mock = MockProvider::spawn().await;
    let mut body = json!({ "model": "deepseek-flash", "tools": tools(), "messages": [
        { "role": "user", "content": USER },
        { "role": "assistant", "content": null, "tool_calls": [{ "id": "c1", "type": "function", "function": { "name": "lookup", "arguments": "{}" } }] },
        { "role": "tool", "tool_call_id": "c1", "content": "42" } ] });
    let (status, error) = json_of(&mock, DEEPSEEK, "", &body).await;
    assert_eq!(status, 400);
    assert!(error["error"]["message"].as_str().unwrap().contains("reasoning_content"), "{error}");
    body["messages"][1]["reasoning_content"] = json!("I should look it up.");
    assert_eq!(json_of(&mock, DEEPSEEK, "", &body).await.0, 200);

    body["tool_choice"] = json!("required");
    assert_eq!(json_of(&mock, DEEPSEEK, "", &body).await.0, 400, "forced tool choice while thinking");
}

#[tokio::test]
async fn glm_streams_usage_without_being_asked_and_has_business_codes() {
    let mock = MockProvider::spawn().await;
    let (_, chunks) = stream_of(&mock, GLM, "", &chat("glm-4.7", true)).await;
    let last = chunks.last().unwrap();
    assert_eq!(last["choices"][0]["finish_reason"], "stop");
    assert!(last["usage"]["prompt_tokens_details"]["cached_tokens"].is_u64(), "{last}");
    assert!(last["usage"]["completion_tokens_details"]["reasoning_tokens"].is_u64(), "{last}");
    assert!(chunks[..chunks.len() - 1].iter().all(|c| c.get("usage").is_none()));

    let (status, error) = json_of(&mock, GLM, "status=429", &chat("glm-4.7", false)).await;
    assert_eq!((status, &error["error"]["code"]), (429, &json!("1302")));
    let (status, error) = json_of(&mock, GLM, "status=429,code=1113", &chat("glm-4.7", false)).await;
    assert_eq!((status, &error["error"]["code"]), (429, &json!("1113")));
}

#[tokio::test]
async fn kimi_puts_stream_usage_in_the_choice_and_fixes_its_samplers() {
    let mock = MockProvider::spawn().await;
    let mut body = chat("kimi-k3", true);
    body["stream_options"] = json!({ "include_usage": true });
    let (_, chunks) = stream_of(&mock, KIMI, "cache_read=2,cache_write=5", &body).await;
    let (own, rest) = chunks.split_last().unwrap();
    assert_eq!(own["choices"], json!([]));
    assert_eq!(own["usage"]["cached_tokens"], 2);
    assert_eq!(own["usage"]["prompt_tokens_details"]["cache_write_tokens"], 5);
    let finish = rest.last().unwrap();
    assert!(finish["choices"][0]["usage"]["total_tokens"].is_u64(), "{finish}");
    assert!(finish["choices"][0]["usage"].get("cached_tokens").is_none());

    let mut hot = chat("kimi-k3", false);
    hot["temperature"] = json!(0.3);
    let (status, error) = json_of(&mock, KIMI, "", &hot).await;
    assert_eq!(status, 400);
    assert_eq!(error["error"]["type"], "invalid_request_error");
    let (_, error) =
        json_of(&mock, KIMI, "status=429,code=exceeded_current_quota_error", &chat("kimi-k3", false)).await;
    assert_eq!(error["error"]["type"], "exceeded_current_quota_error");
}

#[tokio::test]
async fn gemini_openai_signs_tool_calls_and_wraps_errors_in_an_array() {
    let mock = MockProvider::spawn().await;
    let mut body = chat("gemini-3.8-flash", false);
    body["tools"] = tools();
    let (_, answer) = json_of(&mock, GEMINI_OPENAI, "tool=lookup", &body).await;
    let call = answer["choices"][0]["message"]["tool_calls"][0].clone();
    assert!(call["extra_content"]["google"]["thought_signature"].is_string(), "{call}");

    let turn = |call: Value| {
        json!({ "model": "gemini-3.8-flash", "tools": tools(), "messages": [
            { "role": "user", "content": USER },
            { "role": "assistant", "content": null, "tool_calls": [call] },
            { "role": "tool", "tool_call_id": "c1", "content": "42" } ] })
    };
    assert_eq!(json_of(&mock, GEMINI_OPENAI, "", &turn(call.clone())).await.0, 200);
    let mut bare = call;
    bare.as_object_mut().unwrap().remove("extra_content");
    let (status, error) = json_of(&mock, GEMINI_OPENAI, "", &turn(bare)).await;
    assert_eq!(status, 400);
    assert_eq!(error[0]["error"]["status"], "INVALID_ARGUMENT", "{error}");

    let models: Value =
        get(&mock, "/emulate/gemini-openai/v1beta/openai/models").send().await.unwrap().json().await.unwrap();
    assert!(models["data"][0]["id"].as_str().unwrap().starts_with("models/"));
}

#[tokio::test]
async fn ollama_reasons_in_its_own_field_when_asked() {
    let mock = MockProvider::spawn().await;
    let (_, plain) = json_of(&mock, OLLAMA, "", &chat("qwen3:8b", false)).await;
    assert!(plain["choices"][0]["message"].get("reasoning").is_none());
    let mut asked = chat("qwen3:8b", false);
    asked["reasoning_effort"] = json!("low");
    let (_, body) = json_of(&mock, OLLAMA, "", &asked).await;
    assert_eq!(body["choices"][0]["message"]["reasoning"], thinking_text(USER));
    assert!(body["choices"][0]["message"].get("reasoning_content").is_none());
}

#[tokio::test]
async fn native_gemini_checks_signatures_in_the_current_turn() {
    let mock = MockProvider::spawn().await;
    let path = "/emulate/gemini/v1beta/models/gemini-3.8-flash:generateContent";
    let user = json!({ "role": "user", "parts": [{ "text": USER }] });
    let (_, answer) = json_of(&mock, path, "tool=lookup", &json!({ "contents": [user] })).await;
    let model_turn = answer["candidates"][0]["content"].clone();
    assert!(model_turn["parts"][0]["thoughtSignature"].is_string(), "{model_turn}");
    let result = json!({ "role": "user", "parts": [{ "functionResponse": { "name": "lookup", "response": { "result": 42 } } }] });
    let (status, _) = json_of(&mock, path, "", &json!({ "contents": [user, model_turn, result] })).await;
    assert_eq!(status, 200);
    let mut bare = model_turn;
    bare["parts"][0].as_object_mut().unwrap().remove("thoughtSignature");
    let (status, error) = json_of(&mock, path, "", &json!({ "contents": [user, bare, result] })).await;
    assert_eq!(status, 400);
    assert_eq!(error["error"]["status"], "INVALID_ARGUMENT");
}

#[tokio::test]
async fn unknown_paths_under_an_emulation_are_404() {
    let mock = MockProvider::spawn().await;
    let response = post(&mock, "/emulate/glm/api/paas/v4/embeddings").json(&json!({})).send().await.unwrap();
    assert_eq!(response.status(), 404);
}

const GEMINI: &str = "/emulate/gemini/v1beta/models";
const VERTEX: &str = "/emulate/vertex/v1/projects/p/locations/europe-west4/publishers/google/models";

/// A recorded answer's body, for comparing shapes.
fn recorded(provider: &str, case: &str) -> Value {
    let file = format!("{}/fixtures/{provider}/{case}.json", env!("CARGO_MANIFEST_DIR"));
    let fixture: Value = serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    fixture["response"]["body"].clone()
}

/// The member names of an object, sorted.
fn keys(v: &Value) -> Vec<String> {
    let mut k: Vec<String> = v.as_object().map(|m| m.keys().cloned().collect()).unwrap_or_default();
    k.sort();
    k
}

#[tokio::test]
async fn gemini_counts_tokens_and_embeds_as_recorded() {
    let mock = MockProvider::spawn().await;
    let contents = json!([{ "role": "user", "parts": [{ "text": USER }] }]);
    let (status, count) =
        json_of(&mock, &format!("{GEMINI}/gemini-3.8-flash:countTokens"), "", &json!({ "contents": contents })).await;
    assert_eq!(status, 200);
    assert_eq!(keys(&count), keys(&recorded("gemini", "count-tokens")));
    let nested = json!({ "generateContentRequest": { "model": "models/gemini-3.8-flash", "contents": contents,
                         "systemInstruction": { "parts": [{ "text": "Be brief." }] } } });
    let (_, more) = json_of(&mock, &format!("{GEMINI}/gemini-3.8-flash:countTokens"), "", &nested).await;
    assert!(more["totalTokens"].as_u64() > count["totalTokens"].as_u64(), "the system instruction counts: {more}");

    let embed = json!({ "content": { "parts": [{ "text": "a fox" }] }, "outputDimensionality": 128 });
    let (status, one) = json_of(&mock, &format!("{GEMINI}/gemini-embedding-001:embedContent"), "", &embed).await;
    assert_eq!(status, 200);
    assert_eq!(keys(&one), keys(&recorded("gemini", "embed")), "no usage, as recorded");
    assert_eq!(one["embedding"]["values"].as_array().unwrap().len(), 128);

    let request = |text: &str| json!({ "model": "models/gemini-embedding-001", "content": { "parts": [{ "text": text }] }, "outputDimensionality": 8 });
    let (_, batch) = json_of(
        &mock,
        &format!("{GEMINI}/gemini-embedding-001:batchEmbedContents"),
        "",
        &json!({ "requests": [request("a"), request("b")] }),
    )
    .await;
    assert_eq!(keys(&batch), keys(&recorded("gemini", "batch-embed")));
    assert_eq!(batch["embeddings"].as_array().unwrap().len(), 2);
    assert_ne!(batch["embeddings"][0], batch["embeddings"][1], "vectors follow the text");
    let many: Vec<Value> = (0..101).map(|i| request(&i.to_string())).collect();
    let (status, error) =
        json_of(&mock, &format!("{GEMINI}/gemini-embedding-001:batchEmbedContents"), "", &json!({ "requests": many }))
            .await;
    assert_eq!(status, 400);
    assert_eq!(error, recorded("gemini", "batch-embed-101"), "the recorded limit and message");
}

/// A token from the emulated token endpoint, for an assertion signed with the test key.
async fn vertex_token(mock: &MockProvider, query: &str) -> String {
    use pacod_gw_testkit::google::{ServiceAccount, TEST_SERVICE_ACCOUNT, mint};
    let account = ServiceAccount::parse(TEST_SERVICE_ACCOUNT).unwrap();
    let url = format!("{}/emulate/google-oauth/token{query}", mock.base_url());
    mint(&reqwest::Client::new(), &account, &url).await.unwrap()
}

#[tokio::test]
async fn vertex_takes_only_tokens_its_endpoint_issued() {
    let mock = MockProvider::spawn().await;
    let body = json!({ "contents": [{ "role": "user", "parts": [{ "text": USER }] }] });
    let path = format!("{VERTEX}/gemini-3.8-flash:generateContent");
    let refused = post(&mock, &path).bearer_auth("not-issued").json(&body).send().await.unwrap();
    assert_eq!(refused.status(), 401);
    let error: Value = refused.json().await.unwrap();
    assert_eq!(error["error"]["status"], "UNAUTHENTICATED");

    let token = vertex_token(&mock, "?expires_in=120").await;
    let answer: Value = post(&mock, &path).bearer_auth(&token).json(&body).send().await.unwrap().json().await.unwrap();
    assert_eq!(answer["candidates"][0]["content"]["parts"][0]["text"], REPLY);
    assert_eq!(answer["usageMetadata"]["trafficType"], "ON_DEMAND");
    let minted: Value = get(&mock, "/__mock/google").send().await.unwrap().json().await.unwrap();
    assert_eq!(minted["minted"], 1);

    // Express mode: no project in the path, an API key instead of a token.
    let express = post(&mock, "/emulate/vertex/v1/publishers/google/models/gemini-3.8-flash:countTokens")
        .header("x-goog-api-key", "express-key")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(express.status(), 200);
    // Text embeddings through `predict`, and the project-less model list.
    let predict: Value = post(&mock, &format!("{VERTEX}/text-embedding-005:predict"))
        .bearer_auth(&token)
        .json(&json!({ "instances": [{ "content": "a" }, { "content": "b" }], "parameters": { "outputDimensionality": 4 } }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(predict["predictions"].as_array().unwrap().len(), 2);
    let list = get(&mock, "/emulate/vertex/v1beta1/publishers/google/models").bearer_auth(&token).send().await.unwrap();
    assert_eq!(list.status(), 200);
    let other =
        post(&mock, "/emulate/vertex/v1/projects/p/datasets/x").bearer_auth(&token).json(&body).send().await.unwrap();
    assert_eq!(other.status(), 404, "only the model methods are emulated");
}

#[tokio::test]
async fn xai_reasoning_models_refuse_stop_and_count_reasoning_apart() {
    let mock = MockProvider::spawn().await;
    let path = "/emulate/xai/v1/chat/completions";
    let mut body = chat("grok-4", false);
    body["stop"] = json!(["END"]);
    let (status, error) = json_of(&mock, path, "", &body).await;
    assert_eq!(status, 400);
    assert_eq!(error["code"], "invalid-argument", "{error}");
    assert!(error["error"].as_str().unwrap().contains("stop"));
    let (status, ok) = json_of(&mock, path, "reasoning=7", &chat("grok-4", false)).await;
    assert_eq!(status, 200);
    let u = &ok["usage"];
    assert_eq!(u["completion_tokens_details"]["reasoning_tokens"], 7);
    assert_eq!(
        u["total_tokens"].as_u64().unwrap(),
        u["prompt_tokens"].as_u64().unwrap() + u["completion_tokens"].as_u64().unwrap() + 7,
        "outside completion_tokens, inside the total"
    );
    // The mini model takes an effort; the non-reasoning one anything.
    let mut mini = chat("grok-3-mini", false);
    mini["reasoning_effort"] = json!("low");
    assert_eq!(json_of(&mock, path, "", &mini).await.0, 200);
    let mut plain = chat("grok-4-fast-non-reasoning", false);
    plain["stop"] = json!(["END"]);
    assert_eq!(json_of(&mock, path, "", &plain).await.0, 200);
}

#[tokio::test]
async fn a_scripted_reply_replaces_the_echo() {
    use base64::Engine as _;
    let mock = MockProvider::spawn().await;
    let reply = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(r#"{"prices":[]}"#);
    let (_, body) = json_of(&mock, "/v1/chat/completions", &format!("reply={reply}"), &chat("gpt-mock", false)).await;
    assert_eq!(body["choices"][0]["message"]["content"], r#"{"prices":[]}"#);
}
