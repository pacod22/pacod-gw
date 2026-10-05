//! The Anthropic API.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use pacod_gw_testkit::mock::{MockProvider, count_tokens, thinking_text, tool_arguments};
use serde_json::{Value, json};

use crate::support::{Event, REPLY, USER, anthropic_post, get, header, post, sse_events};

const MESSAGES: &str = "/v1/messages";

fn message(stream: bool) -> Value {
    json!({
        "model": "claude-sonnet-5-5",
        "max_tokens": 1024,
        "stream": stream,
        "system": "be brief",
        "messages": [{ "role": "user", "content": USER }],
    })
}

async fn events(mock: &MockProvider, x_mock: &str) -> Vec<Event> {
    let response = anthropic_post(mock, MESSAGES).header("x-mock", x_mock).json(&message(true)).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(header(&response, "content-type"), Some("text/event-stream; charset=utf-8"));
    let events = sse_events(&response.text().await.unwrap());
    // The event name always repeats the `type` of the data.
    for event in &events {
        assert_eq!(event.json()["type"], event.event.as_deref().unwrap());
    }
    events
}

fn names(events: &[Event]) -> Vec<&str> {
    events.iter().map(|event| event.event.as_deref().unwrap()).collect()
}

/// The `delta` objects of the given delta type, in order.
fn deltas(events: &[Event], kind: &str) -> Vec<Value> {
    events
        .iter()
        .map(Event::json)
        .filter(|data| data["type"] == "content_block_delta" && data["delta"]["type"] == kind)
        .map(|data| data["delta"].clone())
        .collect()
}

fn joined(deltas: &[Value], field: &str) -> String {
    deltas.iter().map(|delta| delta[field].as_str().unwrap()).collect()
}

#[tokio::test]
async fn message_response() {
    let mock = MockProvider::spawn().await;
    let response = anthropic_post(&mock, MESSAGES).json(&message(false)).send().await.unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(header(&response, "content-type"), Some("application/json"));
    let mut body: Value = response.json().await.unwrap();

    let id = body["id"].as_str().unwrap().to_owned();
    assert!(id.starts_with("msg_"), "{id}");
    body.as_object_mut().unwrap().remove("id");
    // "be brief" + "hello world": 5 input tokens. "echo: hello world": 5 output tokens.
    assert_eq!(
        body,
        json!({
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-5-5",
            "content": [{ "type": "text", "text": REPLY }],
            "stop_reason": "end_turn",
            "stop_sequence": null,
            "stop_details": null,
            "usage": {
                "input_tokens": 5,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 0,
                    "ephemeral_1h_input_tokens": 0,
                },
                "output_tokens": 5,
                "service_tier": "standard",
            },
        })
    );
}

#[tokio::test]
async fn content_blocks_and_tool_results_count_as_prompt_text() {
    let mock = MockProvider::spawn().await;
    let request = json!({
        "model": "claude-sonnet-5-5",
        "max_tokens": 64,
        "system": [{ "type": "text", "text": "abcd" }],
        "messages": [
            { "role": "user", "content": [{ "type": "text", "text": "efgh" }] },
            { "role": "assistant", "content": [
                { "type": "text", "text": "ijkl" },
                { "type": "tool_use", "id": "toolu_1", "name": "t", "input": {} },
            ]},
            { "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "toolu_1", "content": [
                    { "type": "text", "text": "mnop" },
                ]},
                { "type": "text", "text": "qrst" },
            ]},
        ],
    });
    let body: Value = anthropic_post(&mock, MESSAGES).json(&request).send().await.unwrap().json().await.unwrap();
    // Twenty characters of prompt text; the last user message reads "mnopqrst".
    assert_eq!(body["usage"]["input_tokens"], 5);
    assert_eq!(body["content"][0]["text"], "echo: mnopqrst");

    let counted: Value =
        anthropic_post(&mock, "/v1/messages/count_tokens").json(&request).send().await.unwrap().json().await.unwrap();
    assert_eq!(counted, json!({ "input_tokens": 5 }));
}

#[tokio::test]
async fn stream_event_sequence() {
    let mock = MockProvider::spawn().await;
    let events = events(&mock, "").await;
    assert_eq!(
        names(&events),
        [
            "message_start",
            "content_block_start",
            "ping",
            "content_block_delta",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "ping",
            "message_delta",
            "message_stop",
        ]
    );

    let start = events[0].json();
    let id = start["message"]["id"].as_str().unwrap();
    assert!(id.starts_with("msg_"));
    assert_eq!(
        start,
        json!({ "type": "message_start", "message": {
            "id": id,
            "type": "message",
            "role": "assistant",
            "model": "claude-sonnet-5-5",
            "content": [],
            "stop_reason": null,
            "stop_sequence": null,
            "usage": {
                "input_tokens": 5,
                "cache_creation_input_tokens": 0,
                "cache_read_input_tokens": 0,
                "cache_creation": {
                    "ephemeral_5m_input_tokens": 0,
                    "ephemeral_1h_input_tokens": 0,
                },
                "output_tokens": 1,
                "service_tier": "standard",
            },
        }})
    );
    assert_eq!(
        events[1].json(),
        json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": { "type": "text", "text": "" },
        })
    );
    assert_eq!(events[2].json(), json!({ "type": "ping" }));
    assert_eq!(
        events[3].json(),
        json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": { "type": "text_delta", "text": "echo:" },
        })
    );
    assert_eq!(joined(&deltas(&events, "text_delta"), "text"), REPLY);
    assert_eq!(events[6].json(), json!({ "type": "content_block_stop", "index": 0 }));
    assert_eq!(
        events[8].json(),
        json!({
            "type": "message_delta",
            "delta": { "stop_reason": "end_turn", "stop_sequence": null },
            "usage": { "output_tokens": 5 },
        })
    );
    assert_eq!(events[9].json(), json!({ "type": "message_stop" }));
}

#[tokio::test]
async fn thinking_block_with_signature() {
    let mock = MockProvider::spawn().await;
    let thinking = thinking_text(USER);
    let output_tokens = 5 + count_tokens(&thinking);

    let body: Value = anthropic_post(&mock, MESSAGES)
        .header("x-mock", "thinking")
        .json(&message(false))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let content = body["content"].as_array().unwrap();
    assert_eq!(content.len(), 2);
    assert_eq!(content[0]["type"], "thinking");
    assert_eq!(content[0]["thinking"], thinking);
    let signature = content[0]["signature"].as_str().unwrap();
    assert!(!STANDARD.decode(signature).unwrap().is_empty());
    assert_eq!(content[1], json!({ "type": "text", "text": REPLY }));
    assert_eq!(body["usage"]["output_tokens"], output_tokens);

    let events = events(&mock, "thinking,chunks=2").await;
    assert_eq!(
        names(&events),
        [
            "message_start",
            "content_block_start",
            "ping",
            "content_block_delta",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "content_block_start",
            "content_block_delta",
            "content_block_delta",
            "content_block_stop",
            "ping",
            "message_delta",
            "message_stop",
        ]
    );
    assert_eq!(events[1].json()["content_block"], json!({ "type": "thinking", "thinking": "", "signature": "" }));
    let thinking_deltas = deltas(&events, "thinking_delta");
    assert_eq!(thinking_deltas.len(), 2);
    assert_eq!(joined(&thinking_deltas, "thinking"), thinking);
    // The signature is the last delta of the thinking block, which is block 0.
    let signature_event = events[5].json();
    assert_eq!(signature_event["index"], 0);
    assert_eq!(signature_event["delta"]["type"], "signature_delta");
    assert!(!STANDARD.decode(signature_event["delta"]["signature"].as_str().unwrap()).unwrap().is_empty());
    assert_eq!(events[7].json()["index"], 1);
    assert_eq!(events[7].json()["content_block"]["type"], "text");
    assert_eq!(joined(&deltas(&events, "text_delta"), "text"), REPLY);
    assert_eq!(events[12].json()["usage"], json!({ "output_tokens": output_tokens }));
}

#[tokio::test]
async fn tool_use() {
    let mock = MockProvider::spawn().await;
    let arguments = tool_arguments(USER);

    let body: Value = anthropic_post(&mock, MESSAGES)
        .header("x-mock", "tool=get_weather")
        .json(&message(false))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(body["stop_reason"], "tool_use");
    let block = &body["content"][0];
    assert_eq!(block["type"], "tool_use");
    assert_eq!(block["name"], "get_weather");
    assert_eq!(block["input"], json!({ "input": USER }));
    assert!(block["id"].as_str().unwrap().starts_with("toolu_"));
    assert_eq!(body["usage"]["output_tokens"], count_tokens(&arguments));

    let events = events(&mock, "tool=get_weather,chunks=3").await;
    let start = events[1].json();
    assert_eq!(start["content_block"]["type"], "tool_use");
    assert_eq!(start["content_block"]["name"], "get_weather");
    assert_eq!(start["content_block"]["input"], json!({}));
    assert!(start["content_block"]["id"].as_str().unwrap().starts_with("toolu_"));
    // An empty fragment opens the arguments, then they arrive in three pieces.
    let fragments = deltas(&events, "input_json_delta");
    assert_eq!(fragments.len(), 4);
    assert_eq!(fragments[0]["partial_json"], "");
    assert_eq!(joined(&fragments, "partial_json"), arguments);
    let message_delta = events[events.len() - 2].json();
    assert_eq!(message_delta["delta"]["stop_reason"], "tool_use");
}

#[tokio::test]
async fn stop_reasons() {
    let mock = MockProvider::spawn().await;
    for reason in ["end_turn", "max_tokens", "tool_use", "refusal", "pause_turn"] {
        let x_mock = format!("stop={reason}");
        let body: Value = anthropic_post(&mock, MESSAGES)
            .header("x-mock", &x_mock)
            .json(&message(false))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body["stop_reason"], reason);
        assert_eq!(body["stop_details"].is_null(), reason != "refusal");

        let events = events(&mock, &x_mock).await;
        let message_delta = events[events.len() - 2].json();
        assert_eq!(message_delta["type"], "message_delta");
        assert_eq!(message_delta["delta"]["stop_reason"], reason);
    }
}

#[tokio::test]
async fn missing_headers_are_rejected_like_the_real_api() {
    let mock = MockProvider::spawn().await;

    let no_key =
        post(&mock, MESSAGES).header("anthropic-version", "2023-06-01").json(&message(false)).send().await.unwrap();
    assert_eq!(no_key.status(), 401);
    let request_id = header(&no_key, "request-id").unwrap().to_owned();
    let body: Value = no_key.json().await.unwrap();
    assert_eq!(
        body,
        json!({
            "type": "error",
            "error": {
                "type": "authentication_error",
                "message": "x-api-key header is required",
            },
            "request_id": request_id,
        })
    );

    let no_version =
        post(&mock, MESSAGES).header("x-api-key", "sk-ant-test").json(&message(false)).send().await.unwrap();
    assert_eq!(no_version.status(), 400);
    let body: Value = no_version.json().await.unwrap();
    assert_eq!(body["type"], "error");
    assert_eq!(body["error"]["type"], "invalid_request_error");
    assert_eq!(body["error"]["message"], "anthropic-version: header is required");

    // The same checks guard the other Anthropic endpoints.
    let count = post(&mock, "/v1/messages/count_tokens").json(&message(false)).send().await.unwrap();
    assert_eq!(count.status(), 401);
    let models = get(&mock, "/v1/models").header("x-api-key", "sk-ant-test").send().await.unwrap();
    assert_eq!(models.status(), 400);
    let body: Value = models.json().await.unwrap();
    assert_eq!(body["type"], "error");
}

#[tokio::test]
async fn invalid_requests_are_400() {
    let mock = MockProvider::spawn().await;
    for (request, message) in [
        (json!({ "max_tokens": 1, "messages": [] }), "model: Field required"),
        (json!({ "model": "m", "messages": [] }), "max_tokens: Field required"),
        (json!({ "model": "m", "max_tokens": 1 }), "messages: Field required"),
    ] {
        let response = anthropic_post(&mock, MESSAGES).json(&request).send().await.unwrap();
        assert_eq!(response.status(), 400);
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["error"]["type"], "invalid_request_error");
        assert_eq!(body["error"]["message"], message);
    }

    let response = anthropic_post(&mock, MESSAGES).body("not json").send().await.unwrap();
    assert_eq!(response.status(), 400);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["error"]["type"], "invalid_request_error");
}

#[tokio::test]
async fn count_tokens_endpoint() {
    let mock = MockProvider::spawn().await;
    let response = anthropic_post(&mock, "/v1/messages/count_tokens")
        .json(&json!({
            "model": "claude-sonnet-5-5",
            "system": "be brief",
            "messages": [{ "role": "user", "content": USER }],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body, json!({ "input_tokens": 5 }));
}

#[tokio::test]
async fn models() {
    let mock = MockProvider::spawn().await;
    let response = get(&mock, "/v1/models")
        .header("x-api-key", "sk-ant-test")
        .header("anthropic-version", "2023-06-01")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert!(header(&response, "request-id").is_some());
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["has_more"], false);
    let models = body["data"].as_array().unwrap();
    assert_eq!(body["first_id"], models[0]["id"]);
    assert_eq!(body["last_id"], models[models.len() - 1]["id"]);
    for model in models {
        assert_eq!(model["type"], "model");
        assert!(model["id"].as_str().unwrap().starts_with("claude-"));
        assert!(model["display_name"].is_string() && model["created_at"].is_string());
        assert!(model["max_input_tokens"].as_u64().unwrap() >= 200_000);
        assert!(model["max_tokens"].as_u64().unwrap() >= 64_000);
        let capabilities = &model["capabilities"];
        assert!(capabilities["image_input"]["supported"].is_boolean());
        assert!(capabilities["thinking"]["types"]["adaptive"]["supported"].is_boolean());
        assert!(capabilities["effort"]["max"]["supported"].is_boolean());
    }
}

#[tokio::test]
async fn response_headers() {
    let mock = MockProvider::spawn().await;
    let response = anthropic_post(&mock, MESSAGES).json(&message(false)).send().await.unwrap();
    for kind in ["requests", "tokens", "input-tokens", "output-tokens"] {
        for field in ["limit", "remaining", "reset"] {
            let name = format!("anthropic-ratelimit-{kind}-{field}");
            assert!(header(&response, &name).is_some(), "{name} is missing");
        }
    }
    assert_eq!(header(&response, "anthropic-ratelimit-requests-reset"), Some("2030-01-01T00:00:00Z"));
    assert!(header(&response, "anthropic-organization-id").is_some());
    let generated = header(&response, "request-id").unwrap().to_owned();
    assert!(generated.starts_with("req_"));
    assert!(header(&response, "x-request-id").is_none());

    for stream in [false, true] {
        let pinned = anthropic_post(&mock, MESSAGES)
            .header("x-mock", "request_id=req_pinned_2")
            .json(&message(stream))
            .send()
            .await
            .unwrap();
        assert_eq!(header(&pinned, "request-id"), Some("req_pinned_2"));
    }
}
