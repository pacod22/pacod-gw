//! The scripted behaviours, through the `x-mock` header and through the script queue.

use std::time::{Duration, Instant};

use pacod_gw_testkit::mock::{Behavior, Fail, MockProvider};
use reqwest::{RequestBuilder, Response};
use serde_json::{Value, json};

use crate::support::{BodyEnd, REPLY, USER, anthropic_post, client, header, post, read_body, sse_events, text};

/// How long a stream may stay silent before a test calls it stalled.
const PATIENCE: Duration = Duration::from_millis(750);

fn openai(mock: &MockProvider, stream: bool) -> RequestBuilder {
    post(mock, "/v1/chat/completions").json(&json!({
        "model": "gpt-4o",
        "stream": stream,
        "stream_options": { "include_usage": true },
        "messages": [{ "role": "user", "content": USER }],
    }))
}

fn anthropic(mock: &MockProvider, stream: bool) -> RequestBuilder {
    anthropic_post(mock, "/v1/messages").json(&json!({
        "model": "claude-sonnet-5-5",
        "max_tokens": 1024,
        "stream": stream,
        "messages": [{ "role": "user", "content": USER }],
    }))
}

fn gemini(mock: &MockProvider, stream: bool) -> RequestBuilder {
    let action = if stream { "streamGenerateContent?alt=sse" } else { "generateContent" };
    post(mock, &format!("/v1beta/models/gemini-2.5-flash:{action}")).json(&json!({
        "contents": [{ "role": "user", "parts": [{ "text": USER }] }],
    }))
}

fn ollama(mock: &MockProvider, stream: bool) -> RequestBuilder {
    post(mock, "/api/chat").json(&json!({
        "model": "llama3.2",
        "stream": stream,
        "messages": [{ "role": "user", "content": USER }],
    }))
}

type Api = fn(&MockProvider, bool) -> RequestBuilder;
const APIS: [(&str, Api); 4] = [("openai", openai), ("anthropic", anthropic), ("gemini", gemini), ("ollama", ollama)];

async fn send(request: RequestBuilder, x_mock: &str) -> Response {
    request.header("x-mock", x_mock).send().await.unwrap()
}

async fn json_body(request: RequestBuilder, x_mock: &str) -> Value {
    let response = send(request, x_mock).await;
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}

/// The number of text deltas in (a prefix of) an OpenAI stream.
fn content_chunks(body: &str) -> usize {
    sse_events(body)
        .iter()
        .filter(|event| event.data != "[DONE]")
        .filter(|event| event.json().pointer("/choices/0/delta/content").is_some_and(|c| c != ""))
        .count()
}

#[tokio::test]
async fn delay_holds_back_the_response_head() {
    let mock = MockProvider::spawn().await;
    let started = Instant::now();
    let response = send(openai(&mock, false), "delay=150").await;
    assert!(started.elapsed() >= Duration::from_millis(150));
    assert_eq!(response.status(), 200);

    // Errors are delayed too.
    let started = Instant::now();
    let response = send(openai(&mock, false), "delay=150,status=500").await;
    assert!(started.elapsed() >= Duration::from_millis(150));
    assert_eq!(response.status(), 500);
}

#[tokio::test]
async fn chunks_sets_the_number_of_content_chunks() {
    let mock = MockProvider::spawn().await;
    for count in [1, 2, 8] {
        let body = send(openai(&mock, true), &format!("chunks={count}")).await.text().await.unwrap();
        assert_eq!(content_chunks(&body), count);
        let content: String = sse_events(&body)
            .iter()
            .filter(|event| event.data != "[DONE]")
            .filter_map(|event| {
                event.json().pointer("/choices/0/delta/content").and_then(Value::as_str).map(str::to_owned)
            })
            .collect();
        assert_eq!(content, REPLY);
    }
    // Never more chunks than characters.
    let body = send(openai(&mock, true), "chunks=500").await.text().await.unwrap();
    assert_eq!(content_chunks(&body), REPLY.chars().count());

    // The same option cuts Anthropic text deltas and Ollama lines.
    let body = send(anthropic(&mock, true), "chunks=6").await.text().await.unwrap();
    assert_eq!(body.matches("text_delta").count(), 6);
    let body = send(ollama(&mock, true), "chunks=6").await.text().await.unwrap();
    assert_eq!(body.matches(r#""done":false"#).count(), 6);
}

#[tokio::test]
async fn chunk_delay_spaces_the_stream() {
    let mock = MockProvider::spawn().await;
    let response = send(openai(&mock, true), "chunks=4,chunk_delay=40").await;
    // The clock starts once the head is here: the delays are in the body, not before it.
    let head_received = Instant::now();
    let body = response.text().await.unwrap();
    // Role, four content chunks, finish, usage and the terminator: seven gaps. The first of
    // them may have begun before the head was read.
    assert_eq!(sse_events(&body).len(), 8);
    assert!(head_received.elapsed() >= Duration::from_millis(6 * 40));
}

#[tokio::test]
async fn split_rechunks_without_changing_the_bytes() {
    let mock = MockProvider::spawn().await;
    for (name, api) in APIS {
        let (whole, end) = read_body(send(api(&mock, true), "").await, PATIENCE).await;
        assert_eq!(end, BodyEnd::Complete);
        let whole = whole.concat();

        for size in [1, 7, 64] {
            let response = send(api(&mock, true), &format!("split={size}")).await;
            let (pieces, end) = read_body(response, PATIENCE).await;
            assert_eq!(end, BodyEnd::Complete);
            assert_eq!(pieces.concat(), whole, "{name} with split={size}");
            // Nothing merges the pieces on the way, so events really span chunk boundaries.
            assert!(pieces.iter().all(|piece| piece.len() <= size), "{name} with split={size} sent a larger piece");
        }
    }
}

#[tokio::test]
async fn status_answers_in_the_provider_envelope() {
    let mock = MockProvider::spawn().await;

    let response = send(openai(&mock, true), "status=429,retry_after=3").await;
    assert_eq!(response.status(), 429);
    assert_eq!(header(&response, "retry-after"), Some("3"));
    assert_eq!(header(&response, "content-type"), Some("application/json"));
    assert!(header(&response, "x-request-id").is_some());
    assert!(header(&response, "x-ratelimit-remaining-requests").is_some());
    let body: Value = response.json().await.unwrap();
    assert_eq!(
        body,
        json!({ "error": {
            "message": "mock: scripted error 429",
            "type": "requests",
            "param": null,
            "code": "rate_limit_exceeded",
        }})
    );
    let body: Value = send(openai(&mock, false), "status=500").await.json().await.unwrap();
    assert_eq!(body["error"]["type"], "server_error");

    for (status, kind) in [
        (400, "invalid_request_error"),
        (401, "authentication_error"),
        (403, "permission_error"),
        (404, "not_found_error"),
        (413, "request_too_large"),
        (429, "rate_limit_error"),
        (500, "api_error"),
        (503, "api_error"),
        (529, "overloaded_error"),
    ] {
        let x_mock = format!("status={status},request_id=req_err_{status}");
        let response = send(anthropic(&mock, false), &x_mock).await;
        assert_eq!(response.status().as_u16(), status);
        assert!(header(&response, "retry-after").is_none());
        assert!(header(&response, "anthropic-ratelimit-requests-remaining").is_some());
        let body: Value = response.json().await.unwrap();
        assert_eq!(
            body,
            json!({
                "type": "error",
                "error": { "type": kind, "message": format!("mock: scripted error {status}") },
                "request_id": format!("req_err_{status}"),
            })
        );
    }

    let body: Value = send(gemini(&mock, true), "status=503").await.json().await.unwrap();
    assert_eq!(body["error"], json!({ "code": 503, "message": "mock: scripted error 503", "status": "UNAVAILABLE" }));
    let response = send(ollama(&mock, true), "status=503").await;
    assert_eq!(response.status(), 503);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body, json!({ "error": "mock: scripted error 503" }));
}

#[tokio::test]
async fn fail_in_the_header_fails_n_times_then_succeeds() {
    let mock = MockProvider::spawn().await;
    let first = || openai(&mock, false);
    let second = || anthropic(&mock, false);
    let x_mock = "fail=503x2,retry_after=7";

    // Two different requests keep separate counts, as two logical calls being retried would.
    let response = send(first(), x_mock).await;
    assert_eq!(response.status(), 503);
    assert_eq!(header(&response, "retry-after"), Some("7"));
    assert_eq!(send(second(), x_mock).await.status(), 503);
    assert_eq!(send(first(), x_mock).await.status(), 503);

    let response = send(first(), x_mock).await;
    assert_eq!(response.status(), 200);
    assert!(header(&response, "retry-after").is_none());
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["choices"][0]["message"]["content"], REPLY);

    assert_eq!(send(second(), x_mock).await.status(), 503);
    assert_eq!(send(second(), x_mock).await.status(), 200);

    // After the success the count starts over.
    assert_eq!(send(first(), x_mock).await.status(), 503);
    assert_eq!(mock.captured().len(), 7);
}

#[tokio::test]
async fn fail_in_the_script_covers_the_retries_of_one_call() {
    let mock = MockProvider::spawn().await;
    mock.set_script(Behavior::parse_all(["fail=429x2,retry_after=1,model=served-after-retry", "status=500"]).unwrap());
    assert_eq!(mock.pending_script(), 2);

    for _ in 0..2 {
        // No x-mock header: this is the path for gateways that strip unknown headers.
        let response = anthropic(&mock, false).send().await.unwrap();
        assert_eq!(response.status(), 429);
        assert_eq!(header(&response, "retry-after"), Some("1"));
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["error"]["type"], "rate_limit_error");
        assert_eq!(mock.pending_script(), 2);
    }
    // The entry's other options apply to the request that succeeds.
    let response = anthropic(&mock, false).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert!(header(&response, "retry-after").is_none());
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["model"], "served-after-retry");
    assert_eq!(mock.pending_script(), 1);

    assert_eq!(anthropic(&mock, false).send().await.unwrap().status(), 500);
    assert_eq!(mock.pending_script(), 0);
    // The script is used up: back to normal answers.
    let body: Value = anthropic(&mock, false).send().await.unwrap().json().await.unwrap();
    assert_eq!(body["model"], "claude-sonnet-5-5");
}

#[tokio::test]
async fn script_entries_apply_in_order_and_the_header_wins() {
    let mock = MockProvider::spawn().await;
    mock.set_script([
        Behavior { model: Some("first".into()), stop: Some("length".into()), ..Behavior::default() },
        Behavior { model: Some("second".into()), ..Behavior::default() },
        Behavior { fail: Some(Fail { status: 500, times: 1 }), ..Behavior::default() },
    ]);

    let body = json_body(openai(&mock, false), "").await;
    assert_eq!(body["model"], "first");
    assert_eq!(body["choices"][0]["finish_reason"], "length");

    // The header overrides `model` and adds to the queued entry.
    let body = json_body(openai(&mock, false), "model=from-header,tool=t").await;
    assert_eq!(body["model"], "from-header");
    assert_eq!(body["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(mock.pending_script(), 1);

    // Replacing the script drops what was queued.
    mock.set_script([]);
    assert_eq!(mock.pending_script(), 0);
    assert_eq!(json_body(openai(&mock, false), "").await["model"], "gpt-4o");
}

#[tokio::test]
async fn reset_after_breaks_the_stream() {
    let mock = MockProvider::spawn().await;

    let response = send(openai(&mock, true), "chunks=6,reset_after=2").await;
    assert_eq!(response.status(), 200);
    let (received, end) = read_body(response, PATIENCE).await;
    assert_eq!(end, BodyEnd::Broken);
    let body = text(&received);
    // Everything up to the second content chunk arrived; nothing after it did.
    assert_eq!(content_chunks(&body), 2);
    assert!(!body.contains("[DONE]") && !body.contains(r#""finish_reason":"stop""#));

    // Zero breaks right after the response head.
    let (received, end) = read_body(send(openai(&mock, true), "reset_after=0").await, PATIENCE).await;
    assert_eq!(end, BodyEnd::Broken);
    assert!(received.concat().is_empty());

    // A stream with fewer content chunks than the limit completes.
    let (received, end) = read_body(send(openai(&mock, true), "chunks=2,reset_after=3").await, PATIENCE).await;
    assert_eq!(end, BodyEnd::Complete);
    assert!(text(&received).ends_with("data: [DONE]\n\n"));

    // Anthropic: the break comes inside the text block.
    let (received, end) = read_body(send(anthropic(&mock, true), "chunks=6,reset_after=2").await, PATIENCE).await;
    assert_eq!(end, BodyEnd::Broken);
    let body = text(&received);
    assert_eq!(body.matches("text_delta").count(), 2);
    assert!(body.contains("message_start") && !body.contains("content_block_stop"));

    // Gemini and Ollama: one event or line per content chunk.
    let (received, end) = read_body(send(gemini(&mock, true), "chunks=6,reset_after=2").await, PATIENCE).await;
    assert_eq!(end, BodyEnd::Broken);
    assert_eq!(sse_events(&text(&received)).len(), 2);
    let (received, end) = read_body(send(ollama(&mock, true), "chunks=6,reset_after=2").await, PATIENCE).await;
    assert_eq!(end, BodyEnd::Broken);
    assert_eq!(text(&received).lines().count(), 2);

    // A broken stream with re-chunking still delivers every byte before the break.
    let (split, end) = read_body(send(openai(&mock, true), "chunks=6,reset_after=2,split=5").await, PATIENCE).await;
    assert_eq!(end, BodyEnd::Broken);
    assert_eq!(text(&split), body_of_openai_prefix(&mock).await);
}

/// The bytes an OpenAI stream sends before `reset_after=2` breaks it, without `split`.
async fn body_of_openai_prefix(mock: &MockProvider) -> String {
    let response = send(openai(mock, true), "chunks=6,reset_after=2").await;
    text(&read_body(response, PATIENCE).await.0)
}

#[tokio::test]
async fn hang_never_answers() {
    let mock = MockProvider::spawn().await;
    let error =
        openai(&mock, false).header("x-mock", "hang").timeout(Duration::from_millis(300)).send().await.unwrap_err();
    assert!(error.is_timeout(), "{error:?}");
    // The request was received all the same.
    assert_eq!(mock.captured().len(), 1);

    // Through the script, and the mock keeps serving other requests meanwhile.
    mock.set_script(Behavior::parse_all(["hang"]).unwrap());
    let hanging = tokio::spawn(openai(&mock, false).timeout(Duration::from_millis(300)).send());
    while mock.pending_script() > 0 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(send(openai(&mock, false), "").await.status(), 200);
    assert!(hanging.await.unwrap().unwrap_err().is_timeout());
}

#[tokio::test]
async fn stall_after_goes_silent_without_closing() {
    let mock = MockProvider::spawn().await;
    let response = send(openai(&mock, true), "chunks=6,stall_after=3").await;
    assert_eq!(response.status(), 200);
    let (received, end) = read_body(response, PATIENCE).await;
    assert_eq!(end, BodyEnd::Silent);
    let body = text(&received);
    assert_eq!(content_chunks(&body), 3);
    assert!(!body.contains("[DONE]"));

    let (received, end) = read_body(send(ollama(&mock, true), "stall_after=1").await, PATIENCE).await;
    assert_eq!(end, BodyEnd::Silent);
    assert_eq!(text(&received).lines().count(), 1);

    // The smaller of the two limits decides how the stream ends.
    let (_, end) = read_body(send(openai(&mock, true), "chunks=6,stall_after=4,reset_after=2").await, PATIENCE).await;
    assert_eq!(end, BodyEnd::Broken);
}

#[tokio::test]
async fn no_usage_leaves_usage_out() {
    let mock = MockProvider::spawn().await;

    let body = json_body(openai(&mock, false), "no_usage").await;
    assert!(body.get("usage").is_none());
    let stream = send(openai(&mock, true), "no_usage").await.text().await.unwrap();
    assert!(!stream.contains("usage"));

    let body = json_body(anthropic(&mock, false), "no_usage").await;
    assert!(body.get("usage").is_none());
    let stream = send(anthropic(&mock, true), "no_usage").await.text().await.unwrap();
    assert!(!stream.contains("usage") && stream.contains("message_delta"));

    let body = json_body(gemini(&mock, false), "no_usage").await;
    assert!(body.get("usageMetadata").is_none());
    let stream = send(gemini(&mock, true), "no_usage").await.text().await.unwrap();
    assert!(!stream.contains("usageMetadata"));

    let body = json_body(ollama(&mock, false), "no_usage").await;
    assert!(body.get("eval_count").is_none() && body.get("prompt_eval_count").is_none());
    assert_eq!(body["done"], true);
}

#[tokio::test]
async fn usage_options_set_the_cache_and_reasoning_counts() {
    let mock = MockProvider::spawn().await;
    let x_mock = "cache_read=100,cache_write=50,reasoning=7";
    // "hello world": 3 input tokens. "echo: hello world": 5 output tokens.

    let expected = json!({
        "prompt_tokens": 103,
        "completion_tokens": 12,
        "total_tokens": 115,
        "prompt_tokens_details": { "cached_tokens": 100, "audio_tokens": 0 },
        "completion_tokens_details": {
            "reasoning_tokens": 7,
            "audio_tokens": 0,
            "accepted_prediction_tokens": 0,
            "rejected_prediction_tokens": 0,
        },
    });
    let body = json_body(openai(&mock, false), x_mock).await;
    assert_eq!(body["usage"], expected);
    let stream = send(openai(&mock, true), x_mock).await.text().await.unwrap();
    let events = sse_events(&stream);
    assert_eq!(events[events.len() - 2].json()["usage"], expected);

    let body = json_body(anthropic(&mock, false), x_mock).await;
    assert_eq!(
        body["usage"],
        json!({
            "input_tokens": 3,
            "cache_creation_input_tokens": 50,
            "cache_read_input_tokens": 100,
            "cache_creation": { "ephemeral_5m_input_tokens": 50, "ephemeral_1h_input_tokens": 0 },
            "output_tokens": 12,
            "service_tier": "standard",
        })
    );
    let stream = send(anthropic(&mock, true), x_mock).await.text().await.unwrap();
    let events = sse_events(&stream);
    let start_usage = &events[0].json()["message"]["usage"];
    assert_eq!(start_usage["input_tokens"], 3);
    assert_eq!(start_usage["cache_read_input_tokens"], 100);
    assert_eq!(start_usage["cache_creation_input_tokens"], 50);
    assert_eq!(start_usage["output_tokens"], 1);
    assert_eq!(events[events.len() - 2].json()["usage"], json!({ "output_tokens": 12 }));

    let body = json_body(gemini(&mock, false), x_mock).await;
    assert_eq!(
        body["usageMetadata"],
        json!({
            "promptTokenCount": 103,
            "candidatesTokenCount": 5,
            "totalTokenCount": 115,
            "cachedContentTokenCount": 100,
            "thoughtsTokenCount": 7,
        })
    );

    let body = json_body(ollama(&mock, false), x_mock).await;
    assert_eq!(body["prompt_eval_count"], 3);
    assert_eq!(body["eval_count"], 12);
}

#[tokio::test]
async fn stop_and_model_are_reported_as_given() {
    let mock = MockProvider::spawn().await;
    let x_mock = "stop=length,model=served-model";

    let body = json_body(openai(&mock, false), x_mock).await;
    assert_eq!(body["model"], "served-model");
    assert_eq!(body["choices"][0]["finish_reason"], "length");
    let stream = send(openai(&mock, true), x_mock).await.text().await.unwrap();
    assert!(stream.contains(r#""finish_reason":"length""#));
    assert!(!stream.contains("gpt-4o") && stream.contains("served-model"));

    let body = json_body(anthropic(&mock, false), "stop=max_tokens,model=served-model").await;
    assert_eq!(body["model"], "served-model");
    assert_eq!(body["stop_reason"], "max_tokens");
}

#[tokio::test]
async fn invalid_options_are_rejected_loudly() {
    let mock = MockProvider::spawn().await;
    let response = send(openai(&mock, false), "delay=soon").await;
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["type"], "mock_error");
    assert!(body["error"]["message"].as_str().unwrap().contains("option `delay`"));

    // Option names are case-insensitive and accept dashes; repeated headers add up.
    let response = client()
        .post(format!("{}/v1/chat/completions", mock.base_url()))
        .header("x-mock", "Status=503")
        .header("x-mock", "Retry-After=9")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 503);
    assert_eq!(header(&response, "retry-after"), Some("9"));
}

#[tokio::test]
async fn the_same_request_yields_the_same_bytes() {
    let first = MockProvider::spawn().await;
    let second = MockProvider::spawn().await;
    for (name, api) in APIS {
        for stream in [false, true] {
            for x_mock in ["", "thinking,tool=lookup,chunks=3,cache_read=9"] {
                let mut bodies = Vec::new();
                for mock in [&first, &first, &second] {
                    let response = send(api(mock, stream), x_mock).await;
                    assert_eq!(response.status(), 200);
                    bodies.push(response.bytes().await.unwrap());
                }
                assert!(!bodies[0].is_empty());
                assert_eq!(bodies[0], bodies[1], "{name} stream={stream} `{x_mock}`");
                assert_eq!(bodies[0], bodies[2], "{name} stream={stream} `{x_mock}`");
            }
        }
    }

    // Ids follow the request: another prompt, another id.
    let id = async |user: &str| {
        let response = post(&first, "/v1/chat/completions")
            .json(&json!({ "model": "m", "messages": [{ "role": "user", "content": user }] }))
            .send()
            .await
            .unwrap();
        response.json::<Value>().await.unwrap()["id"].as_str().unwrap().to_owned()
    };
    assert_eq!(id("one").await, id("one").await);
    assert_ne!(id("one").await, id("two").await);
}
