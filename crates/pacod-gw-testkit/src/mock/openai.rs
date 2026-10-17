//! The OpenAI-compatible API: chat completions, models, embeddings and speech.
//!
//! Shapes follow the public API reference. Where the reference leaves room, the comments say
//! what was assumed.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use bytes::Bytes;
use serde_json::{Map, Value, json};

use super::{
    flavour::{Flavour, StreamUsage},
    reply::{CREATED, Output, Turn, Usage, count_tokens, fnv1a},
    server::{ApiError, Call, Reply},
    stream::{Frame, sse},
};

const FINGERPRINT: &str = "fp_mock";

/// The length of an embedding when the request does not ask for `dimensions`. The real default
/// is the model's native size (1536 for `text-embedding-3-small`); a short vector keeps test
/// bodies readable.
const EMBEDDING_DIMENSIONS: u64 = 8;

/// Sent with every response. The rate-limit headers are the documented set; the values are
/// fixed, and the reset values use the duration syntax of the real API (`6ms`, `1s`, `6m0s`).
pub(crate) const HEADERS: &[(&str, &str)] = &[
    ("openai-organization", "mock-org"),
    ("openai-processing-ms", "12"),
    ("openai-version", "2020-10-01"),
    ("x-ratelimit-limit-requests", "10000"),
    ("x-ratelimit-limit-tokens", "2000000"),
    ("x-ratelimit-remaining-requests", "9999"),
    ("x-ratelimit-remaining-tokens", "1999000"),
    ("x-ratelimit-reset-requests", "6ms"),
    ("x-ratelimit-reset-tokens", "30ms"),
];

/// `{"error":{"message","type","param","code"}}`.
///
/// The `type` and `code` values per status are the ones commonly observed; the reference only
/// lists the codes in prose, so treat them as approximate.
pub(crate) fn error_body(status: u16, message: &str) -> Value {
    let (kind, code) = match status {
        401 => ("invalid_request_error", Some("invalid_api_key")),
        429 => ("requests", Some("rate_limit_exceeded")),
        status if status >= 500 => ("server_error", None),
        _ => ("invalid_request_error", None),
    };
    json!({ "error": { "message": message, "type": kind, "param": null, "code": code } })
}

pub(crate) fn chat_completions(call: &Call<'_>, flavour: Flavour) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("you must provide a model parameter"))?;
    let messages = request
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("Missing required parameter: 'messages'."))?;

    let mut prompt = String::new();
    let mut user_text = String::new();
    for message in messages {
        let text = content_text(message.get("content"));
        prompt.push_str(&text);
        if message.get("role").and_then(Value::as_str) == Some("user") {
            user_text = text;
        }
    }
    let structured = structured_reply(request.get("response_format"), &user_text);
    let thinking = flavour.thinks(&request, call.behavior.thinking);
    flavour.check(&request, thinking)?;
    let mut behavior = call.behavior.clone();
    behavior.thinking = thinking;
    let turn = Turn::new(call.seed, model, &prompt, &user_text, structured, &behavior);
    let id = format!("chatcmpl-mock{:016x}", call.seed);
    let call_id = format!("call_mock{:016x}", call.seed);

    if request.get("stream").and_then(Value::as_bool) == Some(true) {
        let include_usage = request.pointer("/stream_options/include_usage").and_then(Value::as_bool) == Some(true);
        return Ok(Reply::Stream {
            content_type: "text/event-stream; charset=utf-8",
            frames: chunks(&turn, &id, &call_id, include_usage, flavour),
        });
    }

    let mut message = Map::new();
    message.insert("role".into(), json!("assistant"));
    match &turn.output {
        Output::Text(text) => {
            message.insert("content".into(), json!(text));
        }
        Output::Tool { .. } => {
            message.insert("content".into(), Value::Null);
        }
    }
    if let Some(thinking) = &turn.thinking {
        // Not part of OpenAI's own API: the field DeepSeek introduced and most
        // OpenAI-compatible servers (vLLM, LiteLLM, ...) use for reasoning text; Ollama's is
        // `reasoning`.
        message.insert(flavour.reasoning_field().into(), json!(thinking));
    }
    if let Output::Tool { name, arguments } = &turn.output {
        message.insert("tool_calls".into(), json!([tool_call(&turn, flavour, call_id.as_str(), name, arguments)]));
    } else if flavour == Flavour::GeminiOpenai {
        // Gemini signs a text answer too, on the message (recorded: gemini-openai/chat).
        message.insert("extra_content".into(), json!({ "google": { "thought_signature": turn.signature() } }));
    }
    message.insert("refusal".into(), Value::Null);
    message.insert("annotations".into(), json!([]));

    let mut body = json!({
        "id": id,
        "object": "chat.completion",
        "created": CREATED,
        "model": turn.model,
        "choices": [{
            "index": 0,
            "message": message,
            "logprobs": null,
            "finish_reason": turn.stop_reason("stop", "tool_calls"),
        }],
    });
    if let Some(usage) = &turn.usage {
        body["usage"] = flavour.usage_body(usage);
    }
    body["service_tier"] = json!(turn.tier.as_deref().unwrap_or("default"));
    body["system_fingerprint"] = json!(FINGERPRINT);
    Ok(Reply::Json(body))
}

/// A whole tool call. Gemini's compatible endpoint adds the thought signature to the first call
/// of a step, in `extra_content.google.thought_signature`.
fn tool_call(turn: &Turn, flavour: Flavour, id: &str, name: &str, arguments: &str) -> Value {
    let mut call = json!({ "id": id, "type": "function", "function": { "name": name, "arguments": arguments } });
    if flavour == Flavour::GeminiOpenai {
        call["extra_content"] = json!({ "google": { "thought_signature": turn.signature() } });
    }
    call
}

/// The text of a message: a string, or the `text` parts of a content array, concatenated.
fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts.iter().filter_map(|part| part.get("text").and_then(Value::as_str)).collect(),
        _ => String::new(),
    }
}

/// The reply for a `response_format` that asks for JSON: `{"echo": <user text>}` for
/// `json_object`, and a document shaped after the schema for `json_schema`.
fn structured_reply(response_format: Option<&Value>, user_text: &str) -> Option<String> {
    let format = response_format?;
    let echo = || json!({ "echo": user_text });
    let document = match format.get("type").and_then(Value::as_str)? {
        "json_object" => echo(),
        "json_schema" if format.pointer("/json_schema/name").and_then(Value::as_str) == Some("intent") => {
            intent_reply(format.pointer("/json_schema/schema").unwrap_or(&Value::Null), user_text)
        }
        "json_schema" => match format.pointer("/json_schema/schema") {
            Some(schema) if schema.get("type").is_some() => from_schema(schema, user_text),
            _ => echo(),
        },
        _ => return None,
    };
    Some(document.to_string())
}

/// The answer to the gateway's intent plugin (`docs/plugin-concept.md`, section 5.4), chosen by
/// keyword so that tests can steer it: the first label whose words appear in the text
/// (`legal_advice` matches "legal advice"), else `unknown`; confidence 0.9 for a match and 0.3
/// without; a flag is 0.95 when the text names its id, 0.05 otherwise; a score its lowest
/// level; a choice its first option.
fn intent_reply(schema: &Value, text: &str) -> Value {
    let text = text.to_lowercase();
    let labels: Vec<&str> = schema
        .pointer("/properties/intent/enum")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let found = labels.iter().find(|l| **l != "unknown" && text.contains(&l.replace('_', " ")));
    let mut answer = json!({
        "intent": found.copied().unwrap_or("unknown"),
        "confidence": if found.is_some() { 0.9 } else { 0.3 },
        "secondary": [],
    });
    if let Some(questions) = schema.pointer("/properties/questions/properties").and_then(Value::as_object) {
        let mut out = serde_json::Map::new();
        for (id, q) in questions {
            let value = match (q.get("type").and_then(Value::as_str), q.pointer("/enum/0")) {
                (_, Some(first)) => first.clone(),
                (Some("integer"), _) => json!(0),
                _ => json!(if text.contains(&id.replace('_', " ")) { 0.95 } else { 0.05 }),
            };
            out.insert(id.clone(), value);
        }
        answer["questions"] = Value::Object(out);
    }
    if schema.pointer("/properties/rationale").is_some() {
        answer["rationale"] = json!("the mock chose by keyword");
    }
    if let Some(fields) = schema.pointer("/properties/fields/properties").and_then(Value::as_object) {
        answer["fields"] = Value::Object(fields.keys().map(|k| (k.clone(), json!(""))).collect());
    }
    answer
}

/// A minimal instance of `schema`: objects get all their `properties`, strings the user text,
/// numbers `0`, booleans `false`, arrays `[]`, enums their first value. `$ref` and
/// combinators are not followed and yield `null`.
fn from_schema(schema: &Value, text: &str) -> Value {
    if let Some(first) = schema.get("enum").and_then(Value::as_array).and_then(|values| values.first()) {
        return first.clone();
    }
    if let Some(constant) = schema.get("const") {
        return constant.clone();
    }
    let kind = match schema.get("type") {
        Some(Value::String(kind)) => kind.as_str(),
        // `["string", "null"]`: take the first.
        Some(Value::Array(kinds)) => kinds.first().and_then(Value::as_str).unwrap_or(""),
        _ => "",
    };
    match kind {
        "object" => {
            let properties = schema.get("properties").and_then(Value::as_object);
            let fields =
                properties.into_iter().flatten().map(|(name, schema)| (name.clone(), from_schema(schema, text)));
            Value::Object(fields.collect())
        }
        "array" => json!([]),
        "string" => json!(text),
        "integer" | "number" => json!(0),
        "boolean" => json!(false),
        _ => Value::Null,
    }
}

/// `prompt_tokens` includes the cached tokens and `completion_tokens` the reasoning tokens, as
/// in the real API where the detail fields are subsets.
pub(crate) fn openai_usage(usage: &Usage) -> Value {
    let prompt = usage.input + usage.cache_read;
    let completion = usage.output + usage.reasoning;
    json!({
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
        "prompt_tokens_details": { "cached_tokens": usage.cache_read, "audio_tokens": 0 },
        "completion_tokens_details": {
            "reasoning_tokens": usage.reasoning,
            "audio_tokens": 0,
            "accepted_prediction_tokens": 0,
            "rejected_prediction_tokens": 0,
        },
    })
}

/// The stream: a role chunk, the content chunks, a chunk with the `finish_reason`, the usage
/// where the provider puts it (OpenAI: a chunk of its own with empty `choices`, when the request
/// asked for it), and `data: [DONE]`.
fn chunks(turn: &Turn, id: &str, call_id: &str, include_usage: bool, flavour: Flavour) -> Vec<Frame> {
    let placement = flavour.stream_usage();
    let reported = turn.usage.as_ref();
    // Whether the chunks before the usage say `"usage": null`, as OpenAI and DeepSeek do when asked.
    let null_usage = include_usage && reported.is_some() && placement == StreamUsage::OwnChunkWhenAsked;
    let chunk = |choices: Value, usage_field: Option<Value>| {
        let mut chunk = json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": CREATED,
            "model": turn.model,
            "service_tier": turn.tier.as_deref().unwrap_or("default"),
            "system_fingerprint": FINGERPRINT,
            "choices": choices,
        });
        if let Some(usage_field) = usage_field {
            chunk["usage"] = usage_field;
        }
        sse(None, &chunk)
    };
    let delta = |delta: Value, finish_reason: Option<&str>| {
        chunk(
            json!([{ "index": 0, "delta": delta, "logprobs": null, "finish_reason": finish_reason }]),
            null_usage.then_some(Value::Null),
        )
    };

    let mut frames = Vec::new();
    if flavour == Flavour::DeepSeek {
        // DeepSeek keeps a waiting connection open with SSE comments.
        frames.push(Frame::meta(": keep-alive\n\n"));
    }
    match &turn.output {
        Output::Text(text) => {
            frames.push(Frame::meta(delta(json!({ "role": "assistant", "content": "", "refusal": null }), None)));
            reasoning_chunks(turn, flavour, &delta, &mut frames);
            for piece in turn.pieces(text) {
                frames.push(Frame::content(delta(json!({ "content": piece }), None)));
            }
        }
        Output::Tool { name, arguments } => {
            let mut head = tool_call(turn, flavour, call_id, name, "");
            head["index"] = json!(0);
            let head = json!([head]);
            if turn.thinking.is_some() {
                frames.push(Frame::meta(delta(json!({ "role": "assistant", "content": null, "refusal": null }), None)));
                reasoning_chunks(turn, flavour, &delta, &mut frames);
                frames.push(Frame::meta(delta(json!({ "tool_calls": head }), None)));
            } else {
                // As the real API does: the role and the head of the call in one chunk.
                frames.push(Frame::meta(delta(
                    json!({ "role": "assistant", "content": null, "tool_calls": head, "refusal": null }),
                    None,
                )));
            }
            for piece in turn.pieces(arguments) {
                frames.push(Frame::content(delta(
                    json!({ "tool_calls": [{ "index": 0, "function": { "arguments": piece } }] }),
                    None,
                )));
            }
        }
    }
    let finish = turn.stop_reason("stop", "tool_calls");
    let mut finish_choice = json!({ "index": 0, "delta": {}, "logprobs": null, "finish_reason": finish });
    if flavour == Flavour::GeminiOpenai && matches!(turn.output, Output::Text(_)) {
        // The signature of a text answer comes on the last chunk (recorded: gemini-openai/chat-stream).
        finish_choice["delta"] =
            json!({ "extra_content": { "google": { "thought_signature": turn.signature() } }, "role": "assistant" });
    }
    let mut finish_usage = null_usage.then_some(Value::Null);
    match (placement, reported) {
        (StreamUsage::OnFinishAlways, Some(u)) => finish_usage = Some(flavour.usage_body(u)),
        (StreamUsage::InChoiceAndWhenAsked, Some(u)) => finish_choice["usage"] = Flavour::choice_usage(u),
        _ => {}
    }
    frames.push(Frame::meta(chunk(json!([finish_choice]), finish_usage)));
    if include_usage
        && matches!(placement, StreamUsage::OwnChunkWhenAsked | StreamUsage::InChoiceAndWhenAsked)
        && let Some(u) = reported
    {
        frames.push(Frame::meta(chunk(json!([]), Some(flavour.usage_body(u)))));
    }
    frames.push(Frame::meta("data: [DONE]\n\n"));
    frames
}

fn reasoning_chunks(
    turn: &Turn,
    flavour: Flavour,
    delta: &impl Fn(Value, Option<&str>) -> Bytes,
    frames: &mut Vec<Frame>,
) {
    if let Some(thinking) = &turn.thinking {
        for piece in turn.pieces(thinking) {
            frames.push(Frame::content(delta(json!({ flavour.reasoning_field(): piece }), None)));
        }
    }
}

/// `GET /v1/models/{id}`.
pub(crate) fn model(id: &str) -> Reply {
    Reply::Json(json!({ "id": id, "object": "model", "created": CREATED, "owned_by": "mock" }))
}

pub(crate) fn models(flavour: Flavour) -> Reply {
    Reply::Json(flavour.models())
}

/// One vector per input, derived from the input text. `encoding_format: "base64"` (what the
/// official SDKs request by default) returns the little-endian `f32` bytes in base64.
pub(crate) fn embeddings(call: &Call<'_>) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    let model = request
        .get("model")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("you must provide a model parameter"))?;
    let inputs: Vec<String> = match request.get("input") {
        Some(Value::String(text)) => vec![text.clone()],
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
            items.iter().filter_map(|item| item.as_str().map(str::to_owned)).collect()
        }
        // Token arrays: one array of arrays, or a single array of numbers.
        Some(Value::Array(items)) if items.iter().all(Value::is_array) => items.iter().map(Value::to_string).collect(),
        Some(input @ Value::Array(_)) => vec![input.to_string()],
        _ => {
            return Err(ApiError::bad_request("Missing required parameter: 'input'."));
        }
    };
    let dimensions = request.get("dimensions").and_then(Value::as_u64).unwrap_or(EMBEDDING_DIMENSIONS).clamp(1, 4096);
    let base64 = request.get("encoding_format").and_then(Value::as_str) == Some("base64");

    let data: Vec<Value> = inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            // Each component is one of -1.000, -0.999, ..., 1.000.
            let components = (0..dimensions).map(|component| {
                let hash = fnv1a(&[input.as_bytes(), &component.to_be_bytes()]);
                (hash % 2001) as i32 - 1000
            });
            let embedding = if base64 {
                let bytes: Vec<u8> = components.flat_map(|milli| (milli as f32 / 1000.0).to_le_bytes()).collect();
                json!(STANDARD.encode(bytes))
            } else {
                components.map(|milli| f64::from(milli) / 1000.0).collect()
            };
            json!({ "object": "embedding", "index": index, "embedding": embedding })
        })
        .collect();

    let mut body = json!({
        "object": "list",
        "data": data,
        "model": call.behavior.model.as_deref().unwrap_or(model),
    });
    if !call.behavior.no_usage {
        let tokens = count_tokens(&inputs.concat());
        body["usage"] = json!({ "prompt_tokens": tokens, "total_tokens": tokens });
    }
    Ok(Reply::Json(body))
}

/// A few bytes that start like the requested container and then depend on the request.
pub(crate) fn speech(call: &Call<'_>) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    let input = request
        .get("input")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("Missing required parameter: 'input'."))?;
    let (content_type, magic): (_, &[u8]) = match request.get("response_format").and_then(Value::as_str) {
        None | Some("mp3") => ("audio/mpeg", &[0xff, 0xfb, 0x90, 0x64]),
        Some("opus") => ("audio/opus", b"OggS"),
        Some("aac") => ("audio/aac", &[0xff, 0xf1]),
        Some("flac") => ("audio/flac", b"fLaC"),
        Some("wav") => ("audio/wav", b"RIFF"),
        Some("pcm") => ("audio/pcm", &[]),
        Some(other) => {
            return Err(ApiError::bad_request(format!(
                "Invalid value: '{other}'. Supported values are: 'mp3', 'opus', 'aac', \
                     'flac', 'wav', and 'pcm'."
            )));
        }
    };
    let voice = request.get("voice").and_then(Value::as_str).unwrap_or("");
    let mut body = magic.to_vec();
    body.extend_from_slice(&fnv1a(&[input.as_bytes()]).to_be_bytes());
    body.extend_from_slice(&fnv1a(&[voice.as_bytes(), input.as_bytes()]).to_be_bytes());
    Ok(Reply::Bytes { content_type, body: body.into() })
}
