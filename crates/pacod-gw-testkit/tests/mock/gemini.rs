//! The Gemini native API.

use pacod_gw_testkit::mock::{MockProvider, count_tokens, thinking_text};
use serde_json::{Value, json};

use crate::support::{REPLY, USER, header, post, sse_events};

const GENERATE: &str = "/v1beta/models/gemini-2.5-flash:generateContent";
const STREAM: &str = "/v1beta/models/gemini-2.5-flash:streamGenerateContent?alt=sse";

fn request() -> Value {
    json!({
        "systemInstruction": { "parts": [{ "text": "be brief" }] },
        "contents": [{ "role": "user", "parts": [{ "text": USER }] }],
    })
}

async fn stream(mock: &MockProvider, x_mock: &str) -> Vec<Value> {
    let response = post(mock, STREAM)
        .header("x-goog-api-key", "test-key")
        .header("x-mock", x_mock)
        .json(&request())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(header(&response, "content-type"), Some("text/event-stream"));
    let body = response.text().await.unwrap();
    assert!(body.ends_with("\r\n\r\n"));
    sse_events(&body).iter().map(|event| event.json()).collect()
}

fn texts(chunks: &[Value], thought: bool) -> String {
    chunks
        .iter()
        .flat_map(|chunk| chunk["candidates"][0]["content"]["parts"].as_array().unwrap())
        .filter(|part| part["thought"].as_bool().unwrap_or(false) == thought)
        .filter_map(|part| part["text"].as_str())
        .collect()
}

#[tokio::test]
async fn generate_content() {
    let mock = MockProvider::spawn().await;
    let response = post(&mock, GENERATE).header("x-goog-api-key", "test-key").json(&request()).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(header(&response, "content-type"), Some("application/json; charset=UTF-8"));
    // Gemini sends neither a request id nor rate-limit headers.
    assert!(header(&response, "x-request-id").is_none());
    let mut body: Value = response.json().await.unwrap();
    assert!(body["responseId"].is_string());
    body.as_object_mut().unwrap().remove("responseId");
    assert_eq!(
        body,
        json!({
            "candidates": [{
                "content": { "parts": [{ "text": REPLY }], "role": "model" },
                "finishReason": "STOP",
                "index": 0,
            }],
            "usageMetadata": {
                "promptTokenCount": 5,
                "candidatesTokenCount": 5,
                "totalTokenCount": 10,
                "cachedContentTokenCount": 0,
                "thoughtsTokenCount": 0,
            },
            "modelVersion": "gemini-2.5-flash",
        })
    );
}

#[tokio::test]
async fn stream_generate_content() {
    let mock = MockProvider::spawn().await;
    let chunks = stream(&mock, "").await;
    assert_eq!(chunks.len(), 3);
    assert_eq!(texts(&chunks, false), REPLY);

    let (last, earlier) = chunks.split_last().unwrap();
    for chunk in earlier {
        assert!(chunk["candidates"][0].get("finishReason").is_none());
        // Before the end, usage only knows the prompt.
        assert_eq!(chunk["usageMetadata"], json!({ "promptTokenCount": 5, "totalTokenCount": 5 }));
        assert_eq!(chunk["modelVersion"], "gemini-2.5-flash");
        assert_eq!(chunk["responseId"], last["responseId"]);
    }
    assert_eq!(last["candidates"][0]["finishReason"], "STOP");
    assert_eq!(
        last["usageMetadata"],
        json!({
            "promptTokenCount": 5,
            "candidatesTokenCount": 5,
            "totalTokenCount": 10,
            "cachedContentTokenCount": 0,
            "thoughtsTokenCount": 0,
        })
    );
}

#[tokio::test]
async fn stream_without_sse_is_a_json_array() {
    let mock = MockProvider::spawn().await;
    let response =
        post(&mock, "/v1beta/models/gemini-2.5-flash:streamGenerateContent").json(&request()).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let chunks: Vec<Value> = response.json().await.unwrap();
    assert_eq!(chunks.len(), 3);
    assert_eq!(texts(&chunks, false), REPLY);
    assert_eq!(chunks[2]["candidates"][0]["finishReason"], "STOP");
}

#[tokio::test]
async fn thoughts_cache_and_function_calls() {
    let mock = MockProvider::spawn().await;
    let thinking = thinking_text(USER);
    let thoughts = count_tokens(&thinking);

    let chunks = stream(&mock, "thinking,cache_read=100").await;
    assert_eq!(texts(&chunks, true), thinking);
    assert_eq!(texts(&chunks, false), REPLY);
    // The cached tokens are part of the prompt count; the thoughts are not part of the
    // candidates count.
    assert_eq!(
        chunks.last().unwrap()["usageMetadata"],
        json!({
            "promptTokenCount": 105,
            "candidatesTokenCount": 5,
            "totalTokenCount": 110 + thoughts,
            "cachedContentTokenCount": 100,
            "thoughtsTokenCount": thoughts,
        })
    );
    let signed: Vec<&Value> = chunks
        .iter()
        .flat_map(|chunk| chunk["candidates"][0]["content"]["parts"].as_array().unwrap())
        .filter(|part| part.get("thoughtSignature").is_some())
        .collect();
    assert_eq!(signed.len(), 1);
    assert_eq!(signed[0]["text"], "echo:");

    let body: Value = post(&mock, GENERATE)
        .header("x-mock", "tool=get_weather,reasoning=9,stop=MAX_TOKENS,model=gemini-served")
        .json(&request())
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        body["candidates"][0]["content"]["parts"],
        json!([{ "functionCall": { "name": "get_weather", "args": { "input": USER } } }])
    );
    assert_eq!(body["candidates"][0]["finishReason"], "MAX_TOKENS");
    assert_eq!(body["modelVersion"], "gemini-served");
    assert_eq!(body["usageMetadata"]["thoughtsTokenCount"], 9);
}

#[tokio::test]
async fn errors_use_the_google_envelope() {
    let mock = MockProvider::spawn().await;
    let response = post(&mock, GENERATE).header("x-mock", "status=429").json(&request()).send().await.unwrap();
    assert_eq!(response.status(), 429);
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body,
        json!({ "error": {
            "code": 429,
            "message": "mock: scripted error 429",
            "status": "RESOURCE_EXHAUSTED",
        }})
    );

    let invalid = post(&mock, GENERATE).json(&json!({})).send().await.unwrap();
    assert_eq!(invalid.status(), 400);
    let body: Value = invalid.json().await.unwrap();
    assert_eq!(body["error"]["status"], "INVALID_ARGUMENT");

    let unknown = post(&mock, "/v1beta/models/gemini-2.5-flash:predict").json(&request()).send().await.unwrap();
    assert_eq!(unknown.status(), 404);
    let body: Value = unknown.json().await.unwrap();
    assert_eq!(body["error"]["status"], "NOT_FOUND");
}
