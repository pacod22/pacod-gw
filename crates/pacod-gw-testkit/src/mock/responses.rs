//! OpenAI's Responses API (`POST /v1/responses`), whole and streamed.
//!
//! The answer follows the request's behaviour as chat completions do: an echo of the last user
//! text, a call of the scripted tool, reasoning with `thinking`. Reasoning is summarised as
//! text, and comes with `encrypted_content` when the request includes
//! `reasoning.encrypted_content`. A built-in `web_search` tool adds one `web_search_call` item.
//! Nothing is stored: `store` and `previous_response_id` are not looked at beyond the request
//! log, where tests read them.

use serde_json::{Value, json};

use super::{
    reply::{CREATED, Output, Turn},
    server::{ApiError, Call, Reply},
    stream::{Frame, sse},
};

/// The text of a message's content: a string, or the text of its parts.
fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => {
            parts.iter().filter_map(|p| p.get("text").and_then(Value::as_str)).collect::<Vec<_>>().join("")
        }
        _ => String::new(),
    }
}

/// All prompt text, and the text of the last user message.
fn texts(request: &Value) -> (String, String) {
    let mut prompt = request.get("instructions").and_then(Value::as_str).unwrap_or_default().to_owned();
    let mut user = String::new();
    match request.get("input") {
        Some(Value::String(s)) => {
            prompt.push_str(s);
            user.clone_from(s);
        }
        Some(Value::Array(items)) => {
            for item in items {
                let text = match item.get("type").and_then(Value::as_str) {
                    Some("function_call_output") => content_text(item.get("output")),
                    Some("function_call") => {
                        item.get("arguments").and_then(Value::as_str).unwrap_or_default().to_owned()
                    }
                    _ => content_text(item.get("content")),
                };
                prompt.push_str(&text);
                if item.get("role").and_then(Value::as_str) == Some("user") {
                    user = text;
                }
            }
        }
        _ => {}
    }
    (prompt, user)
}

fn usage_body(turn: &Turn) -> Value {
    let Some(u) = &turn.usage else { return Value::Null };
    let input = u.input + u.cache_read;
    let output = u.output + u.reasoning;
    json!({
        "input_tokens": input,
        "input_tokens_details": { "cached_tokens": u.cache_read },
        "output_tokens": output,
        "output_tokens_details": { "reasoning_tokens": u.reasoning },
        "total_tokens": input + output,
    })
}

struct Answer {
    id: String,
    items: Vec<Value>,
    status: &'static str,
}

fn answer(call: &Call<'_>, request: &Value, turn: &Turn) -> Answer {
    let id = format!("resp_mock{:016x}", call.seed);
    let mut items = Vec::new();
    let searches = request
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|t| t.iter().any(|x| x["type"].as_str().is_some_and(|k| k.starts_with("web_search"))));
    if searches {
        items.push(
            json!({ "id": format!("ws_mock{:016x}", call.seed), "type": "web_search_call", "status": "completed",
                           "action": { "type": "search", "query": "mock" } }),
        );
    }
    if let Some(thinking) = &turn.thinking {
        let mut item = json!({ "id": format!("rs_mock{:016x}", call.seed), "type": "reasoning",
                               "summary": [{ "type": "summary_text", "text": thinking }] });
        let encrypted = request
            .get("include")
            .and_then(Value::as_array)
            .is_some_and(|i| i.iter().any(|x| x == "reasoning.encrypted_content"));
        if encrypted {
            item["encrypted_content"] = json!(turn.signature());
        }
        items.push(item);
    }
    match &turn.output {
        Output::Text(text) => items.push(json!({
            "id": format!("msg_mock{:016x}", call.seed),
            "type": "message",
            "status": "completed",
            "role": "assistant",
            "content": [{ "type": "output_text", "text": text, "annotations": [], "logprobs": [] }],
        })),
        Output::Tool { name, arguments } => items.push(json!({
            "id": format!("fc_mock{:016x}", call.seed),
            "type": "function_call",
            "status": "completed",
            "arguments": arguments,
            "call_id": format!("call_mock{:016x}", call.seed),
            "name": name,
        })),
    }
    // A scripted stop reason `length` cuts the answer short.
    let status = if turn.stop_reason("stop", "stop") == "length" { "incomplete" } else { "completed" };
    Answer { id, items, status }
}

fn response(request: &Value, turn: &Turn, a: &Answer, status: &str, output: &[Value]) -> Value {
    json!({
        "id": a.id,
        "object": "response",
        "created_at": CREATED,
        "status": status,
        "error": null,
        "incomplete_details": if status == "incomplete" { json!({ "reason": "max_output_tokens" }) } else { Value::Null },
        "instructions": request.get("instructions").cloned().unwrap_or(Value::Null),
        "model": turn.model,
        "output": output,
        "parallel_tool_calls": true,
        "previous_response_id": null,
        "store": request.get("store").cloned().unwrap_or(json!(true)),
        "tools": request.get("tools").cloned().unwrap_or(json!([])),
        "usage": if status == "in_progress" { Value::Null } else { usage_body(turn) },
        "service_tier": turn.tier.as_deref().unwrap_or("default"),
    })
}

/// `POST /v1/responses`.
pub(crate) fn create(call: &Call<'_>) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("Missing required parameter: 'model'."))?;
    if request.get("input").is_none() {
        return Err(ApiError::bad_request("Missing required parameter: 'input'."));
    }
    let (prompt, user) = texts(&request);
    let turn = Turn::new(call.seed, model, &prompt, &user, None, call.behavior);
    let a = answer(call, &request, &turn);
    if request.get("stream").and_then(Value::as_bool) != Some(true) {
        return Ok(Reply::Json(response(&request, &turn, &a, a.status, &a.items)));
    }

    let mut frames = Vec::new();
    let mut n = 0u64;
    let mut event = |kind: &str, mut body: Value, content: bool| {
        body["type"] = json!(kind);
        body["sequence_number"] = json!(n);
        n += 1;
        let bytes = sse(Some(kind), &body);
        frames.push(if content { Frame::content(bytes) } else { Frame::meta(bytes) });
    };
    event("response.created", json!({ "response": response(&request, &turn, &a, "in_progress", &[]) }), false);
    event("response.in_progress", json!({ "response": response(&request, &turn, &a, "in_progress", &[]) }), false);
    for (index, item) in a.items.iter().enumerate() {
        let id = item["id"].clone();
        match item["type"].as_str() {
            Some("message") => {
                let mut open = item.clone();
                open["status"] = json!("in_progress");
                open["content"] = json!([]);
                event("response.output_item.added", json!({ "output_index": index, "item": open }), false);
                let part = json!({ "type": "output_text", "text": "", "annotations": [], "logprobs": [] });
                event(
                    "response.content_part.added",
                    json!({ "item_id": id, "output_index": index, "content_index": 0, "part": part }),
                    false,
                );
                let text = item["content"][0]["text"].as_str().unwrap_or_default();
                for piece in turn.pieces(text) {
                    event(
                        "response.output_text.delta",
                        json!({ "item_id": id, "output_index": index, "content_index": 0, "delta": piece, "logprobs": [] }),
                        true,
                    );
                }
                event(
                    "response.output_text.done",
                    json!({ "item_id": id, "output_index": index, "content_index": 0, "text": text, "logprobs": [] }),
                    false,
                );
                event(
                    "response.content_part.done",
                    json!({ "item_id": id, "output_index": index, "content_index": 0, "part": item["content"][0] }),
                    false,
                );
            }
            Some("reasoning") => {
                event(
                    "response.output_item.added",
                    json!({ "output_index": index, "item": { "id": id, "type": "reasoning", "summary": [] } }),
                    false,
                );
                let text = item["summary"][0]["text"].as_str().unwrap_or_default();
                event(
                    "response.reasoning_summary_part.added",
                    json!({ "item_id": id, "output_index": index, "summary_index": 0, "part": { "type": "summary_text", "text": "" } }),
                    false,
                );
                for piece in turn.pieces(text) {
                    event(
                        "response.reasoning_summary_text.delta",
                        json!({ "item_id": id, "output_index": index, "summary_index": 0, "delta": piece }),
                        true,
                    );
                }
                event(
                    "response.reasoning_summary_text.done",
                    json!({ "item_id": id, "output_index": index, "summary_index": 0, "text": text }),
                    false,
                );
                event(
                    "response.reasoning_summary_part.done",
                    json!({ "item_id": id, "output_index": index, "summary_index": 0, "part": item["summary"][0] }),
                    false,
                );
            }
            Some("function_call") => {
                let mut open = item.clone();
                open["status"] = json!("in_progress");
                open["arguments"] = json!("");
                event("response.output_item.added", json!({ "output_index": index, "item": open }), false);
                let arguments = item["arguments"].as_str().unwrap_or_default();
                for piece in turn.pieces(arguments) {
                    event(
                        "response.function_call_arguments.delta",
                        json!({ "item_id": id, "output_index": index, "delta": piece }),
                        true,
                    );
                }
                event(
                    "response.function_call_arguments.done",
                    json!({ "item_id": id, "output_index": index, "arguments": arguments }),
                    false,
                );
            }
            _ => event("response.output_item.added", json!({ "output_index": index, "item": item }), false),
        }
        event("response.output_item.done", json!({ "output_index": index, "item": item }), false);
    }
    let last = if a.status == "incomplete" { "response.incomplete" } else { "response.completed" };
    event(last, json!({ "response": response(&request, &turn, &a, a.status, &a.items) }), false);
    Ok(Reply::Stream { content_type: "text/event-stream; charset=utf-8", frames })
}
