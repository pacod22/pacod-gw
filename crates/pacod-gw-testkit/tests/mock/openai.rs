//! The OpenAI-compatible API.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use pacod_gw_testkit::mock::{MockProvider, count_tokens, thinking_text, tool_arguments};
use serde_json::{Value, json};

use crate::support::{REPLY, USER, get, header, post, sse_events};

const CHAT: &str = "/v1/chat/completions";

fn chat(stream: bool) -> Value {
    json!({
        "model": "gpt-4o",
        "stream": stream,
        "messages": [
            { "role": "system", "content": "be brief" },
            { "role": "user", "content": USER },
        ],
    })
}

/// The `data:` payloads of a chat stream, with the terminator checked and removed.
async fn chunks(mock: &MockProvider, x_mock: &str, body: &Value) -> Vec<Value> {
    let response = post(mock, CHAT).header("x-mock", x_mock).json(body).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(header(&response, "content-type"), Some("text/event-stream; charset=utf-8"));
    let events = sse_events(&response.text().await.unwrap());
    let (done, chunks) = events.split_last().expect("a stream has events");
    assert_eq!(done.data, "[DONE]");
    assert!(events.iter().all(|event| event.event.is_none()));
    chunks.iter().map(|event| event.json()).collect()
}

fn deltas(chunks: &[Value], field: &str) -> Vec<String> {
    chunks
        .iter()
        .filter_map(|chunk| chunk.pointer(&format!("/choices/0/delta/{field}")))
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}

#[tokio::test]
async fn chat_completion() {
    let mock = MockProvider::spawn().await;
    let response = post(&mock, CHAT).json(&chat(false)).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(header(&response, "content-type"), Some("application/json"));
    let mut body: Value = response.json().await.unwrap();

    let id = body["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("chatcmpl-"), "{id}");
    body.as_object_mut().unwrap().remove("id");
    // "be brief" + "hello world" is 19 characters: 5 tokens. The reply has 17: 5 tokens.
    assert_eq!(
        body,
        json!({
            "object": "chat.completion",
            "created": 1_700_000_000,
            "model": "gpt-4o",
            "choices": [{
                "index": 0,
                "message": {
                    "role": "assistant",
                    "content": REPLY,
                    "refusal": null,
                    "annotations": [],
                },
                "logprobs": null,
                "finish_reason": "stop",
            }],
            "usage": {
                "prompt_tokens": 5,
                "completion_tokens": 5,
                "total_tokens": 10,
                "prompt_tokens_details": { "cached_tokens": 0, "audio_tokens": 0 },
                "completion_tokens_details": {
                    "reasoning_tokens": 0,
                    "audio_tokens": 0,
                    "accepted_prediction_tokens": 0,
                    "rejected_prediction_tokens": 0,
                },
            },
            "service_tier": "default",
            "system_fingerprint": "fp_mock",
        })
    );
    assert_eq!(count_tokens("be briefhello world"), 5);
    assert_eq!(count_tokens(REPLY), 5);
}

#[tokio::test]
async fn text_parts_and_missing_fields() {
    let mock = MockProvider::spawn().await;
    let parts = json!({
        "model": "gpt-4o",
        "messages": [{ "role": "user", "content": [
            { "type": "text", "text": "hello " },
            { "type": "image_url", "image_url": { "url": "data:image/png;base64,AAAA" } },
            { "type": "text", "text": "world" },
        ]}],
    });
    let body: Value = post(&mock, CHAT).json(&parts).send().await.unwrap().json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], REPLY);

    for (invalid, message) in [
        (json!({ "messages": [] }), "you must provide a model parameter"),
        (json!({ "model": "gpt-4o" }), "Missing required parameter: 'messages'."),
    ] {
        let response = post(&mock, CHAT).json(&invalid).send().await.unwrap();
        assert_eq!(response.status(), 400);
        let body: Value = response.json().await.unwrap();
        assert_eq!(
            body,
            json!({ "error": {
                "message": message,
                "type": "invalid_request_error",
                "param": null,
                "code": null,
            }})
        );
    }
}

#[tokio::test]
async fn stream_without_usage() {
    let mock = MockProvider::spawn().await;
    let chunks = chunks(&mock, "", &chat(true)).await;

    // A role chunk, one chunk per word, and the chunk with the finish reason.
    assert_eq!(chunks.len(), 5);
    for chunk in &chunks {
        assert_eq!(chunk["object"], "chat.completion.chunk");
        assert_eq!(chunk["id"], chunks[0]["id"]);
        assert_eq!(chunk["model"], "gpt-4o");
        assert_eq!(chunk["created"], 1_700_000_000);
        assert!(chunk.get("usage").is_none(), "{chunk}");
        assert_eq!(chunk["choices"].as_array().unwrap().len(), 1);
    }
    assert_eq!(
        chunks[0]["choices"][0],
        json!({
            "index": 0,
            "delta": { "role": "assistant", "content": "", "refusal": null },
            "logprobs": null,
            "finish_reason": null,
        })
    );
    assert_eq!(deltas(&chunks[1..], "content"), ["echo:", " hello", " world"]);
    assert_eq!(chunks[4]["choices"][0], json!({ "index": 0, "delta": {}, "logprobs": null, "finish_reason": "stop" }));
    assert!(chunks[..4].iter().all(|chunk| chunk["choices"][0]["finish_reason"].is_null()));
}

#[tokio::test]
async fn stream_has_a_usage_chunk_only_when_asked() {
    let mock = MockProvider::spawn().await;
    let mut body = chat(true);

    body["stream_options"] = json!({ "include_usage": false });
    let without = chunks(&mock, "", &body).await;
    assert!(without.iter().all(|chunk| chunk.get("usage").is_none()));
    assert!(without.iter().all(|chunk| chunk["choices"] != json!([])));

    body["stream_options"] = json!({ "include_usage": true });
    let with = chunks(&mock, "", &body).await;
    assert_eq!(with.len(), without.len() + 1);
    let (usage, others) = with.split_last().unwrap();
    // Every other chunk announces the field with null.
    for chunk in others {
        assert!(chunk["usage"].is_null() && chunk.get("usage").is_some());
    }
    assert_eq!(others.last().unwrap()["choices"][0]["finish_reason"], "stop");
    assert_eq!(usage["choices"], json!([]));
    assert_eq!(usage["object"], "chat.completion.chunk");
    assert_eq!(usage["usage"]["prompt_tokens"], 5);
    assert_eq!(usage["usage"]["completion_tokens"], 5);
    assert_eq!(usage["usage"]["total_tokens"], 10);
    assert_eq!(usage["usage"]["prompt_tokens_details"]["cached_tokens"], 0);
    assert_eq!(usage["usage"]["completion_tokens_details"]["reasoning_tokens"], 0);

    // `no_usage` wins over the request.
    let suppressed = chunks(&mock, "no_usage", &body).await;
    assert_eq!(suppressed.len(), without.len());
    assert!(suppressed.iter().all(|chunk| chunk.get("usage").is_none()));
}

#[tokio::test]
async fn reasoning_content() {
    let mock = MockProvider::spawn().await;
    let thinking = thinking_text(USER);
    let reasoning_tokens = count_tokens(&thinking);

    let body: Value =
        post(&mock, CHAT).header("x-mock", "thinking").json(&chat(false)).send().await.unwrap().json().await.unwrap();
    let message = &body["choices"][0]["message"];
    assert_eq!(message["reasoning_content"], thinking);
    assert_eq!(message["content"], REPLY);
    assert_eq!(body["usage"]["completion_tokens_details"]["reasoning_tokens"], reasoning_tokens);
    assert_eq!(body["usage"]["completion_tokens"], 5 + reasoning_tokens);

    let mut request = chat(true);
    request["stream_options"] = json!({ "include_usage": true });
    let chunks = chunks(&mock, "thinking", &request).await;
    assert_eq!(deltas(&chunks, "reasoning_content").concat(), thinking);
    assert_eq!(deltas(&chunks, "content").concat(), REPLY);
    // Reasoning comes before the answer.
    let first_content = chunks
        .iter()
        .position(|chunk| chunk.pointer("/choices/0/delta/content").is_some_and(|content| content != ""))
        .unwrap();
    let last_reasoning =
        chunks.iter().rposition(|chunk| chunk.pointer("/choices/0/delta/reasoning_content").is_some()).unwrap();
    assert!(last_reasoning < first_content);
    assert_eq!(chunks.last().unwrap()["usage"]["completion_tokens_details"]["reasoning_tokens"], reasoning_tokens);
}

#[tokio::test]
async fn tool_call() {
    let mock = MockProvider::spawn().await;
    let arguments = tool_arguments(USER);
    assert_eq!(arguments, r#"{"input":"hello world"}"#);

    let body: Value = post(&mock, CHAT)
        .header("x-mock", "tool=get_weather")
        .json(&chat(false))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let choice = &body["choices"][0];
    assert_eq!(choice["finish_reason"], "tool_calls");
    assert!(choice["message"]["content"].is_null());
    let call = &choice["message"]["tool_calls"][0];
    assert_eq!(call["type"], "function");
    assert_eq!(call["function"]["name"], "get_weather");
    assert_eq!(call["function"]["arguments"], arguments);
    assert!(call["id"].as_str().unwrap().starts_with("call_"));
    assert_eq!(body["usage"]["completion_tokens"], count_tokens(&arguments));

    let chunks = chunks(&mock, "tool=get_weather,chunks=4", &chat(true)).await;
    // The first chunk names the call; the fragments only carry arguments.
    let head = &chunks[0]["choices"][0]["delta"];
    let call_id = head["tool_calls"][0]["id"].as_str().unwrap();
    assert!(call_id.starts_with("call_"));
    assert_eq!(
        *head,
        json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "index": 0,
                "id": call_id,
                "type": "function",
                "function": { "name": "get_weather", "arguments": "" },
            }],
            "refusal": null,
        })
    );
    let fragments: Vec<&str> = chunks[1..]
        .iter()
        .filter_map(|chunk| chunk.pointer("/choices/0/delta/tool_calls/0"))
        .map(|call| {
            assert_eq!(call["index"], 0);
            assert!(call.get("id").is_none());
            call["function"]["arguments"].as_str().unwrap()
        })
        .collect();
    assert_eq!(fragments.len(), 4);
    assert_eq!(fragments.concat(), arguments);
    assert_eq!(chunks.last().unwrap()["choices"][0]["finish_reason"], "tool_calls");
}

#[tokio::test]
async fn response_format() {
    let mock = MockProvider::spawn().await;
    let content = async |response_format: Value| {
        let mut request = chat(false);
        request["response_format"] = response_format;
        let body: Value = post(&mock, CHAT).json(&request).send().await.unwrap().json().await.unwrap();
        body["choices"][0]["message"]["content"].as_str().unwrap().to_owned()
    };

    assert_eq!(content(json!({ "type": "text" })).await, REPLY);
    assert_eq!(content(json!({ "type": "json_object" })).await, r#"{"echo":"hello world"}"#);
    let schema = json!({
        "type": "json_schema",
        "json_schema": { "name": "answer", "strict": true, "schema": {
            "type": "object",
            "properties": {
                "title": { "type": "string" },
                "score": { "type": "integer" },
                "ok": { "type": "boolean" },
                "tags": { "type": "array", "items": { "type": "string" } },
                "level": { "enum": ["low", "high"] },
                "nested": { "type": "object", "properties": { "note": { "type": ["string", "null"] } } },
            },
        }},
    });
    assert_eq!(
        serde_json::from_str::<Value>(&content(schema).await).unwrap(),
        json!({
            "title": USER,
            "score": 0,
            "ok": false,
            "tags": [],
            "level": "low",
            "nested": { "note": USER },
        })
    );
}

#[tokio::test]
async fn models() {
    let mock = MockProvider::spawn().await;
    let response = get(&mock, "/v1/models").bearer_auth("sk-test").send().await.unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["object"], "list");
    let models = body["data"].as_array().unwrap();
    assert!(!models.is_empty());
    for model in models {
        assert_eq!(model["object"], "model");
        assert!(model["id"].is_string() && model["owned_by"].is_string());
        assert!(model["created"].is_u64());
    }
    assert!(models.iter().any(|model| model["id"] == "gpt-4o"));
}

#[tokio::test]
async fn embeddings() {
    let mock = MockProvider::spawn().await;
    let embed = async |request: Value| -> Value {
        let response = post(&mock, "/v1/embeddings").json(&request).send().await.unwrap();
        assert_eq!(response.status(), 200);
        response.json().await.unwrap()
    };

    let body = embed(json!({ "model": "text-embedding-3-small", "input": ["abcd", "efghi"] })).await;
    assert_eq!(body["object"], "list");
    assert_eq!(body["model"], "text-embedding-3-small");
    // Nine characters: three tokens.
    assert_eq!(body["usage"], json!({ "prompt_tokens": 3, "total_tokens": 3 }));
    let data = body["data"].as_array().unwrap();
    assert_eq!(data.len(), 2);
    for (index, item) in data.iter().enumerate() {
        assert_eq!(item["object"], "embedding");
        assert_eq!(item["index"], index);
        let vector = item["embedding"].as_array().unwrap();
        assert_eq!(vector.len(), 8);
        assert!(vector.iter().all(|value| (-1.0..=1.0).contains(&value.as_f64().unwrap())));
    }
    assert_ne!(data[0]["embedding"], data[1]["embedding"]);

    // A single string gives the same vector as the same string in a list; `dimensions` sets
    // the length.
    let single = embed(json!({ "model": "m", "input": "abcd", "dimensions": 3 })).await;
    let vector = single["data"][0]["embedding"].as_array().unwrap();
    assert_eq!(vector[..], data[0]["embedding"].as_array().unwrap()[..3]);

    // The base64 form holds the same numbers as little-endian f32.
    let encoded = embed(json!({
        "model": "m", "input": "abcd", "dimensions": 3, "encoding_format": "base64",
    }))
    .await;
    let bytes = STANDARD.decode(encoded["data"][0]["embedding"].as_str().unwrap()).unwrap();
    let (words, rest) = bytes.as_chunks::<4>();
    assert!(rest.is_empty());
    let decoded: Vec<f32> = words.iter().map(|word| f32::from_le_bytes(*word)).collect();
    let expected: Vec<f32> = vector.iter().map(|value| value.as_f64().unwrap() as f32).collect();
    assert_eq!(decoded, expected);

    let response = post(&mock, "/v1/embeddings").json(&json!({ "model": "m" })).send().await.unwrap();
    assert_eq!(response.status(), 400);
}

#[tokio::test]
async fn speech() {
    let mock = MockProvider::spawn().await;
    let speak = async |request: Value| {
        let response = post(&mock, "/v1/audio/speech").json(&request).send().await.unwrap();
        assert_eq!(response.status(), 200);
        let content_type = header(&response, "content-type").unwrap().to_owned();
        (content_type, response.bytes().await.unwrap())
    };

    let request = json!({ "model": "tts-1", "voice": "alloy", "input": "hello" });
    let (content_type, audio) = speak(request.clone()).await;
    assert_eq!(content_type, "audio/mpeg");
    assert_eq!(audio.len(), 20);
    assert_eq!(audio[..2], [0xff, 0xfb]);
    assert_eq!(speak(request).await.1, audio);

    let (content_type, wav) = speak(json!({
        "model": "tts-1", "voice": "alloy", "input": "hello", "response_format": "wav",
    }))
    .await;
    assert_eq!(content_type, "audio/wav");
    assert!(wav.starts_with(b"RIFF"));

    let other = speak(json!({ "model": "tts-1", "voice": "alloy", "input": "goodbye" })).await;
    assert_ne!(other.1, audio);
}

#[tokio::test]
async fn response_headers() {
    let mock = MockProvider::spawn().await;
    let response = post(&mock, CHAT).json(&chat(false)).send().await.unwrap();
    for name in [
        "x-ratelimit-limit-requests",
        "x-ratelimit-limit-tokens",
        "x-ratelimit-remaining-requests",
        "x-ratelimit-remaining-tokens",
        "x-ratelimit-reset-requests",
        "x-ratelimit-reset-tokens",
        "openai-organization",
        "openai-processing-ms",
        "openai-version",
    ] {
        assert!(header(&response, name).is_some(), "{name} is missing");
    }
    assert_eq!(header(&response, "x-ratelimit-limit-requests"), Some("10000"));
    let generated = header(&response, "x-request-id").unwrap().to_owned();
    assert!(generated.starts_with("req_") && generated.len() > 20);
    assert!(header(&response, "request-id").is_none());

    // A generated id is new for every response; a scripted one is sent as given, on a
    // stream too.
    let again = post(&mock, CHAT).json(&chat(false)).send().await.unwrap();
    assert_ne!(header(&again, "x-request-id"), Some(generated.as_str()));
    for stream in [false, true] {
        let pinned =
            post(&mock, CHAT).header("x-mock", "request_id=req_pinned_1").json(&chat(stream)).send().await.unwrap();
        assert_eq!(header(&pinned, "x-request-id"), Some("req_pinned_1"));
        assert!(header(&pinned, "x-ratelimit-remaining-tokens").is_some());
    }
}

#[tokio::test]
async fn unknown_routes_are_404() {
    let mock = MockProvider::spawn().await;
    for response in [post(&mock, "/v1/nope").body("{}").send().await.unwrap(), get(&mock, CHAT).send().await.unwrap()] {
        assert_eq!(response.status(), 404);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["error"]["type"], "invalid_request_error");
    }
    assert_eq!(mock.captured().len(), 2);
}
