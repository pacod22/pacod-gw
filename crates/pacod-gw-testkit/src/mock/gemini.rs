//! The Gemini native API: `generateContent` and `streamGenerateContent`.
//!
//! Shapes follow the public `GenerateContentResponse` reference. Where the reference leaves
//! room, the comments say what was assumed.

use serde_json::{Map, Value, json};

use super::{
    reply::{Output, Turn, Usage},
    server::{ApiError, Call, Reply},
    stream::Frame,
};

/// `{"error":{"code","message","status"}}`, with the canonical gRPC status name.
pub(crate) fn error_body(status: u16, message: &str) -> Value {
    let name = match status {
        400 => "INVALID_ARGUMENT",
        401 => "UNAUTHENTICATED",
        403 => "PERMISSION_DENIED",
        404 => "NOT_FOUND",
        429 => "RESOURCE_EXHAUSTED",
        500 => "INTERNAL",
        503 => "UNAVAILABLE",
        504 => "DEADLINE_EXCEEDED",
        _ => "UNKNOWN",
    };
    json!({ "error": { "code": status, "message": message, "status": name } })
}

/// `POST /v1beta/models/{model}:generateContent` and `:streamGenerateContent`. `strict` adds
/// Gemini 3's checks: thought signatures on function calls of the current turn, no trailing
/// model turn; and every function call it answers carries a signature.
pub(crate) fn generate(call: &Call<'_>, strict: bool) -> Result<Reply, ApiError> {
    let not_found = || ApiError::new(404, format!("mock: no route for POST {}", call.path));
    let (model, action) =
        call.path.strip_prefix("/v1beta/models/").and_then(|target| target.split_once(':')).ok_or_else(not_found)?;
    let streaming = match action {
        "generateContent" => false,
        "streamGenerateContent" => true,
        _ => return Err(not_found()),
    };

    let request = call.json_object()?;
    let contents = request
        .get("contents")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("contents is not specified"))?;
    if strict {
        super::flavour::Flavour::check_gemini(contents)?;
    }
    let mut prompt = String::new();
    // The REST API accepts both spellings.
    let system = request.get("systemInstruction").or_else(|| request.get("system_instruction"));
    parts_text(system, &mut prompt);
    let mut user_text = String::new();
    for content in contents {
        let mut text = String::new();
        parts_text(Some(content), &mut text);
        prompt.push_str(&text);
        // A content without a role is a user turn.
        if matches!(content.get("role").and_then(Value::as_str), None | Some("user")) {
            user_text = text;
        }
    }
    let turn = Turn::new(call.seed, model, &prompt, &user_text, None, call.behavior);
    let response_id = format!("mock{:016x}", call.seed);
    let finish_reason = turn.stop_reason("STOP", "STOP");

    // The parts of the answer in order; each becomes one stream chunk.
    let mut parts: Vec<Map<String, Value>> = Vec::new();
    if let Some(thinking) = &turn.thinking {
        for piece in pieces(&turn, thinking, streaming) {
            parts.push(part(json!({ "text": piece, "thought": true })));
        }
    }
    let answer_start = parts.len();
    match &turn.output {
        Output::Text(text) => {
            for piece in pieces(&turn, text, streaming) {
                parts.push(part(json!({ "text": piece })));
            }
        }
        Output::Tool { name, arguments } => {
            let args: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
            parts.push(part(json!({ "functionCall": { "name": name, "args": args } })));
        }
    }
    let calls = matches!(turn.output, Output::Tool { .. });
    if (turn.thinking.is_some() || (strict && calls))
        && let Some(first) = parts.get_mut(answer_start)
    {
        // Assumed placement: the API attaches the signature of the preceding thoughts to
        // the first part that follows them.
        first.insert("thoughtSignature".into(), json!(turn.signature()));
    }

    let response = |parts: Vec<Map<String, Value>>, last: bool| {
        let mut candidate = json!({ "content": { "parts": parts, "role": "model" } });
        if last {
            candidate["finishReason"] = json!(finish_reason);
        }
        candidate["index"] = json!(0);
        let mut response = json!({ "candidates": [candidate] });
        if let Some(usage) = &turn.usage {
            response["usageMetadata"] = usage_body(usage, last);
        }
        response["modelVersion"] = json!(turn.model);
        response["responseId"] = json!(response_id);
        response
    };

    if !streaming {
        return Ok(Reply::Json(response(parts, true)));
    }
    let sse = call.query.split('&').any(|pair| pair == "alt=sse");
    let count = parts.len();
    let mut frames: Vec<Frame> = parts
        .into_iter()
        .enumerate()
        .map(|(index, part)| {
            let chunk = response(vec![part], index + 1 == count).to_string();
            Frame::content(if sse {
                // The real API ends its events with CRLF pairs.
                format!("data: {chunk}\r\n\r\n")
            } else if index == 0 {
                format!("[{chunk}")
            } else {
                format!("\n,\r\n{chunk}")
            })
        })
        .collect();
    if sse {
        Ok(Reply::Stream { content_type: "text/event-stream", frames })
    } else {
        // Without `alt=sse` the stream is one JSON array, written element by element.
        frames.push(Frame::meta("]"));
        Ok(Reply::Stream { content_type: "application/json; charset=UTF-8", frames })
    }
}

fn part(value: Value) -> Map<String, Value> {
    match value {
        Value::Object(map) => map,
        _ => Map::new(),
    }
}

/// One part per content chunk when streaming, one part for the whole string otherwise.
fn pieces(turn: &Turn, text: &str, streaming: bool) -> Vec<String> {
    if streaming { turn.pieces(text) } else { vec![text.to_owned()] }
}

/// The text of the `parts` of a content.
fn parts_text(content: Option<&Value>, out: &mut String) {
    let parts = content.and_then(|content| content.get("parts")).and_then(Value::as_array);
    for part in parts.into_iter().flatten() {
        if let Some(text) = part.get("text").and_then(Value::as_str) {
            out.push_str(text);
        }
    }
}

/// `promptTokenCount` includes the cached tokens; `candidatesTokenCount` excludes the
/// thoughts, which `totalTokenCount` adds. All four requested counts are always present,
/// whereas the real API leaves zero-valued ones out.
///
/// Every stream chunk carries `usageMetadata`, as in the real API; before the last chunk it
/// only holds the prompt count.
fn usage_body(usage: &Usage, last: bool) -> Value {
    let prompt = usage.input + usage.cache_read;
    if !last {
        return json!({ "promptTokenCount": prompt, "totalTokenCount": prompt });
    }
    json!({
        "promptTokenCount": prompt,
        "candidatesTokenCount": usage.output,
        "totalTokenCount": prompt + usage.output + usage.reasoning,
        "cachedContentTokenCount": usage.cache_read,
        "thoughtsTokenCount": usage.reasoning,
    })
}
