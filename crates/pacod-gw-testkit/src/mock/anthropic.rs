//! The Anthropic API: messages, token counting and models.
//!
//! Shapes follow the public API reference (Messages, Streaming, Errors, Rate limits, Models).
//! Where the reference leaves room, the comments say what was assumed.

use axum::http::HeaderMap;
use serde_json::{Value, json};

use super::{
    reply::{Output, Turn, Usage, count_tokens},
    server::{ApiError, Call, Reply},
    stream::{Frame, sse},
};

/// Sent with every response: the documented rate-limit headers with fixed values (the reset
/// times are RFC 3339, as documented) and the organisation id.
pub(crate) const HEADERS: &[(&str, &str)] = &[
    ("anthropic-organization-id", "00000000-0000-4000-8000-000000000000"),
    ("anthropic-ratelimit-requests-limit", "4000"),
    ("anthropic-ratelimit-requests-remaining", "3999"),
    ("anthropic-ratelimit-requests-reset", "2030-01-01T00:00:00Z"),
    ("anthropic-ratelimit-input-tokens-limit", "2000000"),
    ("anthropic-ratelimit-input-tokens-remaining", "1999000"),
    ("anthropic-ratelimit-input-tokens-reset", "2030-01-01T00:00:00Z"),
    ("anthropic-ratelimit-output-tokens-limit", "400000"),
    ("anthropic-ratelimit-output-tokens-remaining", "399000"),
    ("anthropic-ratelimit-output-tokens-reset", "2030-01-01T00:00:00Z"),
    ("anthropic-ratelimit-tokens-limit", "2400000"),
    ("anthropic-ratelimit-tokens-remaining", "2398000"),
    ("anthropic-ratelimit-tokens-reset", "2030-01-01T00:00:00Z"),
];

/// `{"type":"error","error":{"type","message"},"request_id"}`. The `request_id` repeats the
/// `request-id` response header, as in the real API.
pub(crate) fn error_body(status: u16, message: &str, request_id: &str) -> Value {
    let kind = match status {
        401 => "authentication_error",
        402 => "billing_error",
        403 => "permission_error",
        404 => "not_found_error",
        413 => "request_too_large",
        429 => "rate_limit_error",
        504 => "timeout_error",
        529 => "overloaded_error",
        status if status >= 500 => "api_error",
        _ => "invalid_request_error",
    };
    json!({
        "type": "error",
        "error": { "type": kind, "message": message },
        "request_id": request_id,
    })
}

/// The checks the API makes before it looks at the request: 401 without `x-api-key`, 400
/// without `anthropic-version`.
///
/// The real API also accepts an OAuth token in `Authorization: Bearer`; the mock insists on
/// `x-api-key`, which is what a gateway holding a provider API key must send.
pub(crate) fn authorize(headers: &HeaderMap) -> Result<(), ApiError> {
    if !headers.contains_key("x-api-key") {
        return Err(ApiError::new(401, "x-api-key header is required"));
    }
    if !headers.contains_key("anthropic-version") {
        return Err(ApiError::bad_request("anthropic-version: header is required"));
    }
    Ok(())
}

pub(crate) fn messages(call: &Call<'_>) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    let model = required_model(&request)?;
    if !request.get("max_tokens").is_some_and(Value::is_u64) {
        return Err(ApiError::bad_request("max_tokens: Field required"));
    }
    let (prompt, user_text) = prompt_text(&request)?;
    let turn = Turn::new(call.seed, model, &prompt, &user_text, None, call.behavior);
    let id = format!("msg_mock{:016x}", call.seed);
    let tool_id = format!("toolu_mock{:016x}", call.seed);
    let stop_reason = turn.stop_reason("end_turn", "tool_use");

    if request.get("stream").and_then(Value::as_bool) == Some(true) {
        return Ok(Reply::Stream {
            content_type: "text/event-stream; charset=utf-8",
            frames: events(&turn, &id, &tool_id, stop_reason),
        });
    }

    let mut content = Vec::new();
    if let Some(thinking) = &turn.thinking {
        content.push(json!({
            "type": "thinking",
            "thinking": thinking,
            "signature": turn.signature(),
        }));
    }
    content.push(match &turn.output {
        Output::Text(text) => json!({ "type": "text", "text": text }),
        Output::Tool { name, arguments } => json!({
            "type": "tool_use",
            "id": tool_id,
            "name": name,
            "input": tool_input(arguments),
        }),
    });
    let mut body = json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": turn.model,
        "content": content,
        "stop_reason": stop_reason,
        "stop_sequence": null,
        "stop_details": stop_details(stop_reason),
    });
    if let Some(usage) = &turn.usage {
        body["usage"] = usage_body(usage, usage.output + usage.reasoning);
        if let Some(tier) = &turn.tier {
            body["usage"]["service_tier"] = json!(tier);
        }
    }
    Ok(Reply::Json(body))
}

/// `POST /v1/messages/count_tokens`: the same input count a message request would report.
pub(crate) fn count_message_tokens(call: &Call<'_>) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    required_model(&request)?;
    let (prompt, _) = prompt_text(&request)?;
    Ok(Reply::Json(json!({ "input_tokens": count_tokens(&prompt) })))
}

fn required_model(request: &Value) -> Result<&str, ApiError> {
    request.get("model").and_then(Value::as_str).ok_or_else(|| ApiError::bad_request("model: Field required"))
}

/// All prompt text (the system prompt, then every message) and the text of the last user
/// message.
fn prompt_text(request: &Value) -> Result<(String, String), ApiError> {
    let messages = request
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("messages: Field required"))?;
    let mut prompt = String::new();
    block_text(request.get("system"), &mut prompt);
    let mut user_text = String::new();
    for message in messages {
        let mut text = String::new();
        block_text(message.get("content"), &mut text);
        prompt.push_str(&text);
        if message.get("role").and_then(Value::as_str) == Some("user") {
            user_text = text;
        }
    }
    Ok((prompt, user_text))
}

/// The text of a content value: a string, or the `text` of each block of an array. Blocks
/// with nested content (`tool_result`) contribute the text inside.
fn block_text(content: Option<&Value>, out: &mut String) {
    match content {
        Some(Value::String(text)) => out.push_str(text),
        Some(Value::Array(blocks)) => {
            for block in blocks {
                if let Some(text) = block.get("text").and_then(Value::as_str) {
                    out.push_str(text);
                } else {
                    block_text(block.get("content"), out);
                }
            }
        }
        _ => {}
    }
}

fn tool_input(arguments: &str) -> Value {
    serde_json::from_str(arguments).unwrap_or_else(|_| json!({}))
}

/// `stop_details` is `null` unless the stop reason is `refusal`. The reference names the
/// fields (`type`, `category`, `explanation`); a mock refusal has no category to report, so
/// both are `null` here.
fn stop_details(stop_reason: &str) -> Value {
    if stop_reason == "refusal" {
        json!({ "type": "refusal", "category": null, "explanation": null })
    } else {
        Value::Null
    }
}

/// `input_tokens` is the uncached remainder: the cache counts are reported next to it, not
/// inside it. `cache_creation` breaks the written tokens down by TTL; the mock books them all
/// under the default five minutes.
fn usage_body(usage: &Usage, output_tokens: u64) -> Value {
    json!({
        "input_tokens": usage.input,
        "cache_creation_input_tokens": usage.cache_write,
        "cache_read_input_tokens": usage.cache_read,
        "cache_creation": {
            "ephemeral_5m_input_tokens": usage.cache_write,
            "ephemeral_1h_input_tokens": 0,
        },
        "output_tokens": output_tokens,
        "service_tier": "standard",
    })
}

/// The event sequence of a streamed message.
///
/// `message_start` carries the input usage and `output_tokens: 1`; `message_delta` carries
/// the final, cumulative `output_tokens` (the documented minimum: the real API may repeat
/// the input counts there). A `ping` follows the first `content_block_start` and another
/// precedes `message_delta`.
fn events(turn: &Turn, id: &str, tool_id: &str, stop_reason: &str) -> Vec<Frame> {
    let event = |name: &str, data: Value| sse(Some(name), &data);
    let delta = |index: usize, delta: Value| {
        event("content_block_delta", json!({ "type": "content_block_delta", "index": index, "delta": delta }))
    };
    let mut frames = Vec::new();

    let mut message = json!({
        "id": id,
        "type": "message",
        "role": "assistant",
        "model": turn.model,
        "content": [],
        "stop_reason": null,
        "stop_sequence": null,
    });
    if let Some(usage) = &turn.usage {
        message["usage"] = usage_body(usage, 1);
        if let Some(tier) = &turn.tier {
            message["usage"]["service_tier"] = json!(tier);
        }
    }
    frames.push(Frame::meta(event("message_start", json!({ "type": "message_start", "message": message }))));

    let mut index = 0;
    let mut start = |frames: &mut Vec<Frame>, block: Value| {
        frames.push(Frame::meta(event(
            "content_block_start",
            json!({ "type": "content_block_start", "index": index, "content_block": block }),
        )));
        if index == 0 {
            frames.push(Frame::meta(event("ping", json!({ "type": "ping" }))));
        }
        index += 1;
        index - 1
    };
    let stop = |frames: &mut Vec<Frame>, index: usize| {
        frames.push(Frame::meta(event("content_block_stop", json!({ "type": "content_block_stop", "index": index }))));
    };

    if let Some(thinking) = &turn.thinking {
        let index = start(&mut frames, json!({ "type": "thinking", "thinking": "", "signature": "" }));
        for piece in turn.pieces(thinking) {
            frames.push(Frame::content(delta(index, json!({ "type": "thinking_delta", "thinking": piece }))));
        }
        frames.push(Frame::meta(delta(index, json!({ "type": "signature_delta", "signature": turn.signature() }))));
        stop(&mut frames, index);
    }
    match &turn.output {
        Output::Text(text) => {
            let index = start(&mut frames, json!({ "type": "text", "text": "" }));
            for piece in turn.pieces(text) {
                frames.push(Frame::content(delta(index, json!({ "type": "text_delta", "text": piece }))));
            }
            stop(&mut frames, index);
        }
        Output::Tool { name, arguments } => {
            let index = start(&mut frames, json!({ "type": "tool_use", "id": tool_id, "name": name, "input": {} }));
            // The real API opens the arguments with an empty fragment.
            frames.push(Frame::meta(delta(index, json!({ "type": "input_json_delta", "partial_json": "" }))));
            for piece in turn.pieces(arguments) {
                frames.push(Frame::content(delta(index, json!({ "type": "input_json_delta", "partial_json": piece }))));
            }
            stop(&mut frames, index);
        }
    }

    frames.push(Frame::meta(event("ping", json!({ "type": "ping" }))));
    let mut stop_delta = json!({ "stop_reason": stop_reason, "stop_sequence": null });
    if stop_reason == "refusal" {
        // Assumed to arrive with the stop reason; the reference does not show the event.
        stop_delta["stop_details"] = stop_details(stop_reason);
    }
    let mut message_delta = json!({ "type": "message_delta", "delta": stop_delta });
    if let Some(usage) = &turn.usage {
        message_delta["usage"] = json!({ "output_tokens": usage.output + usage.reasoning });
    }
    frames.push(Frame::meta(event("message_delta", message_delta)));
    frames.push(Frame::meta(event("message_stop", json!({ "type": "message_stop" }))));
    frames
}

/// `GET /v1/models/{id}`.
pub(crate) fn model(id: &str) -> Reply {
    Reply::Json(json!({
        "type": "model",
        "id": id,
        "display_name": id,
        "created_at": "2026-01-01T00:00:00Z",
        "max_input_tokens": 200_000,
        "max_tokens": 64_000,
    }))
}

/// `GET /v1/models`. `max_input_tokens`, `max_tokens` and `capabilities` are the fields the
/// Models API added in 2026; the capability tree is the part the reference shows (every leaf
/// has `supported`), not the full tree, and `created_at` is a placeholder date.
pub(crate) fn models() -> Reply {
    let model = |id: &str, name: &str, max_input_tokens: u64, max_tokens: u64, adaptive: bool| {
        let level = || json!({ "supported": adaptive });
        json!({
            "type": "model",
            "id": id,
            "display_name": name,
            "created_at": "2026-01-01T00:00:00Z",
            "max_input_tokens": max_input_tokens,
            "max_tokens": max_tokens,
            "capabilities": {
                "image_input": { "supported": true },
                "structured_outputs": { "supported": true },
                "thinking": {
                    "supported": true,
                    "types": {
                        "enabled": { "supported": !adaptive },
                        "adaptive": { "supported": adaptive },
                    },
                },
                "effort": {
                    "supported": adaptive,
                    "low": level(),
                    "medium": level(),
                    "high": level(),
                    "max": level(),
                },
            },
        })
    };
    Reply::Json(json!({
        "data": [
            model("claude-opus-5-5", "Claude Opus 5.5", 1_000_000, 128_000, true),
            model("claude-sonnet-5-5", "Claude Sonnet 5.5", 1_000_000, 128_000, true),
            model("claude-haiku-4-5", "Claude Haiku 4.5", 200_000, 64_000, false),
        ],
        "has_more": false,
        "first_id": "claude-opus-5-5",
        "last_id": "claude-haiku-4-5",
    }))
}
