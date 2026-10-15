//! Options in the path (`/x-mock/<options>/…`), for gateways that forward no `x-mock` header:
//! the full gateway's managed mode calls `{base_url}/…`, so a provider whose base URL carries
//! the options gets them on every call.

use std::time::{Duration, Instant};

use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

use crate::support::{USER, get, post, sse_events};

fn chat(stream: bool) -> Value {
    json!({ "model": "gpt-4o", "stream": stream, "messages": [{ "role": "user", "content": USER }] })
}

#[tokio::test]
async fn options_in_the_path_apply_and_the_rest_is_routed_as_usual() {
    let mock = MockProvider::spawn().await;
    let started = Instant::now();
    let response =
        post(&mock, "/x-mock/delay=200,chunks=4/v1/chat/completions").json(&chat(true)).send().await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(200));
    assert_eq!(response.status(), 200);
    let body = response.text().await.unwrap();
    let deltas = sse_events(&body)
        .iter()
        .filter(|e| e.data != "[DONE]")
        .filter(|e| e.json()["choices"][0]["delta"]["content"].as_str().is_some_and(|c| !c.is_empty()))
        .count();
    assert_eq!(deltas, 4);
    // Captured as sent, prefix included.
    assert_eq!(mock.captured()[0].path, "/x-mock/delay=200,chunks=4/v1/chat/completions");
}

#[tokio::test]
async fn the_prefix_combines_with_an_emulation_and_with_the_model_probe() {
    let mock = MockProvider::spawn().await;
    let gemini = post(&mock, "/x-mock/delay=0/emulate/gemini/v1beta/models/gemini-2.5-flash:generateContent")
        .json(&json!({ "contents": [{ "role": "user", "parts": [{ "text": USER }] }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(gemini.status(), 200);
    let body: Value = gemini.json().await.unwrap();
    assert!(body["candidates"][0]["content"]["parts"][0]["text"].is_string(), "{body}");

    let probe = get(&mock, "/x-mock/delay=0/v1/models/gpt-mock").send().await.unwrap();
    assert_eq!(probe.status(), 200);
}

#[tokio::test]
async fn the_header_wins_over_the_path() {
    let mock = MockProvider::spawn().await;
    let started = Instant::now();
    let response = post(&mock, "/x-mock/delay=2000/v1/chat/completions")
        .header("x-mock", "delay=0")
        .json(&chat(false))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(started.elapsed() < Duration::from_millis(1000));
}

#[tokio::test]
async fn bad_options_in_the_path_are_refused() {
    let mock = MockProvider::spawn().await;
    let response = post(&mock, "/x-mock/delay=soon/v1/chat/completions").json(&chat(false)).send().await.unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["type"], "mock_error");
    assert!(body["error"]["message"].as_str().unwrap().contains("in the path"), "{body}");
}
