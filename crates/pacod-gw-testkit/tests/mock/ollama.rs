//! The Ollama native API.

use pacod_gw_testkit::mock::{MockProvider, count_tokens, thinking_text};
use serde_json::{Value, json};

use crate::support::{REPLY, USER, get, header, post};

const CHAT: &str = "/api/chat";

fn request(stream: Option<bool>) -> Value {
    let mut request = json!({
        "model": "llama3.2",
        "messages": [
            { "role": "system", "content": "be brief" },
            { "role": "user", "content": USER },
        ],
    });
    if let Some(stream) = stream {
        request["stream"] = json!(stream);
    }
    request
}

async fn lines(mock: &MockProvider, x_mock: &str, request: &Value) -> Vec<Value> {
    let response = post(mock, CHAT).header("x-mock", x_mock).json(request).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(header(&response, "content-type"), Some("application/x-ndjson"));
    let body = response.text().await.unwrap();
    assert!(body.ends_with('\n'));
    body.lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

#[tokio::test]
async fn chat_streams_by_default() {
    let mock = MockProvider::spawn().await;
    // No `stream` field at all: Ollama streams.
    let lines = lines(&mock, "", &request(None)).await;
    assert_eq!(lines.len(), 4);

    let (last, pieces) = lines.split_last().unwrap();
    for piece in pieces {
        assert_eq!(piece["model"], "llama3.2");
        assert_eq!(piece["done"], false);
        assert_eq!(piece["message"]["role"], "assistant");
        assert!(piece["created_at"].is_string());
        assert!(piece.get("eval_count").is_none());
    }
    let content: String = pieces.iter().map(|piece| piece["message"]["content"].as_str().unwrap()).collect();
    assert_eq!(content, REPLY);
    assert_eq!(
        *last,
        json!({
            "model": "llama3.2",
            "created_at": "2024-01-01T00:00:00Z",
            "message": { "role": "assistant", "content": "" },
            "done_reason": "stop",
            "done": true,
            "total_duration": 1_000_000_000,
            "load_duration": 10_000_000,
            "prompt_eval_count": 5,
            "prompt_eval_duration": 100_000_000,
            "eval_count": 5,
            "eval_duration": 800_000_000,
        })
    );
    assert_eq!(lines, self::lines(&mock, "", &request(Some(true))).await);
}

#[tokio::test]
async fn chat_as_one_object() {
    let mock = MockProvider::spawn().await;
    let response = post(&mock, CHAT).json(&request(Some(false))).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(header(&response, "content-type"), Some("application/json; charset=utf-8"));
    assert!(header(&response, "x-request-id").is_none());
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["message"], json!({ "role": "assistant", "content": REPLY }));
    assert_eq!(body["done"], true);
    assert_eq!(body["done_reason"], "stop");
    assert_eq!(body["prompt_eval_count"], 5);
    assert_eq!(body["eval_count"], 5);
}

#[tokio::test]
async fn thinking_tools_and_options() {
    let mock = MockProvider::spawn().await;
    let thinking = thinking_text(USER);

    let lines = lines(&mock, "thinking,chunks=2", &request(None)).await;
    assert_eq!(lines.len(), 5);
    let thought: String = lines.iter().filter_map(|line| line["message"]["thinking"].as_str()).collect();
    assert_eq!(thought, thinking);
    assert_eq!(lines[4]["eval_count"], 5 + count_tokens(&thinking));

    let body: Value = post(&mock, CHAT)
        .header("x-mock", "tool=get_weather,stop=length,model=served,no_usage")
        .json(&request(Some(false)))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["model"], "served");
    assert_eq!(body["done_reason"], "length");
    // Ollama passes tool arguments as an object.
    assert_eq!(
        body["message"]["tool_calls"],
        json!([{ "function": { "name": "get_weather", "arguments": { "input": USER } } }])
    );
    assert!(body.get("prompt_eval_count").is_none());
    assert!(body.get("eval_count").is_none());
}

#[tokio::test]
async fn tags_and_version() {
    let mock = MockProvider::spawn().await;
    let tags: Value = get(&mock, "/api/tags").send().await.unwrap().json().await.unwrap();
    let models = tags["models"].as_array().unwrap();
    assert!(!models.is_empty());
    for model in models {
        assert_eq!(model["name"], model["model"]);
        assert!(model["size"].is_u64() && model["digest"].is_string());
        assert!(model["details"]["family"].is_string());
    }

    let version: Value = get(&mock, "/api/version").send().await.unwrap().json().await.unwrap();
    assert!(version["version"].is_string());
}

#[tokio::test]
async fn errors_are_a_bare_message() {
    let mock = MockProvider::spawn().await;
    let response = post(&mock, CHAT).header("x-mock", "status=500").json(&request(None)).send().await.unwrap();
    assert_eq!(response.status(), 500);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body, json!({ "error": "mock: scripted error 500" }));

    let invalid = post(&mock, CHAT).json(&json!({})).send().await.unwrap();
    assert_eq!(invalid.status(), 400);
    let body: Value = invalid.json().await.unwrap();
    assert_eq!(body, json!({ "error": "model is required" }));
}
