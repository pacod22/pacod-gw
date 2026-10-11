//! The Ollama native API: `/api/chat`, `/api/tags` and `/api/version`.
//!
//! Shapes follow Ollama's API documentation. Timestamps and durations are fixed.

use serde_json::{Map, Value, json};

use super::{
    reply::{Output, Turn},
    server::{ApiError, Call, Reply},
    stream::Frame,
};

const CREATED_AT: &str = "2024-01-01T00:00:00Z";

/// `{"error": "<message>"}`.
pub(crate) fn error_body(message: &str) -> Value {
    json!({ "error": message })
}

/// `POST /api/chat`. Ollama streams unless the request says `"stream": false`: one JSON
/// object per line, the last one with `done: true` and the counts.
pub(crate) fn chat(call: &Call<'_>) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    let model =
        request.get("model").and_then(Value::as_str).ok_or_else(|| ApiError::bad_request("model is required"))?;
    let no_messages = Vec::new();
    let messages = request.get("messages").and_then(Value::as_array).unwrap_or(&no_messages);
    let mut prompt = String::new();
    let mut user_text = String::new();
    for message in messages {
        let text = message.get("content").and_then(Value::as_str).unwrap_or("");
        prompt.push_str(text);
        if message.get("role").and_then(Value::as_str) == Some("user") {
            text.clone_into(&mut user_text);
        }
    }
    let turn = Turn::new(call.seed, model, &prompt, &user_text, None, call.behavior);

    let line = |message: Map<String, Value>, done: bool| {
        let mut line = json!({
            "model": turn.model,
            "created_at": CREATED_AT,
            "message": message,
        });
        if done {
            line["done_reason"] = json!(turn.stop_reason("stop", "stop"));
        }
        line["done"] = json!(done);
        if done {
            line["total_duration"] = json!(1_000_000_000u64);
            line["load_duration"] = json!(10_000_000u64);
            if let Some(usage) = &turn.usage {
                line["prompt_eval_count"] = json!(usage.input);
            }
            line["prompt_eval_duration"] = json!(100_000_000u64);
            if let Some(usage) = &turn.usage {
                line["eval_count"] = json!(usage.output + usage.reasoning);
            }
            line["eval_duration"] = json!(800_000_000u64);
        }
        line
    };
    // An assistant message with the given extra field.
    let message = |content: &str, extra: Option<(&str, Value)>| {
        let mut message = Map::new();
        message.insert("role".into(), json!("assistant"));
        message.insert("content".into(), json!(content));
        if let Some((key, value)) = extra {
            message.insert(key.into(), value);
        }
        message
    };
    let tool_calls = |name: &str, arguments: &str| {
        // Ollama passes the arguments as an object, in one piece.
        let arguments: Value = serde_json::from_str(arguments).unwrap_or_else(|_| json!({}));
        json!([{ "function": { "name": name, "arguments": arguments } }])
    };

    if request.get("stream").and_then(Value::as_bool) == Some(false) {
        let mut whole = match &turn.output {
            Output::Text(text) => message(text, None),
            Output::Tool { name, arguments } => message("", Some(("tool_calls", tool_calls(name, arguments)))),
        };
        if let Some(thinking) = &turn.thinking {
            whole.insert("thinking".into(), json!(thinking));
        }
        return Ok(Reply::Json(line(whole, true)));
    }

    let ndjson = |line: Value| format!("{line}\n");
    let mut frames = Vec::new();
    if let Some(thinking) = &turn.thinking {
        for piece in turn.pieces(thinking) {
            let message = message("", Some(("thinking", json!(piece))));
            frames.push(Frame::content(ndjson(line(message, false))));
        }
    }
    match &turn.output {
        Output::Text(text) => {
            for piece in turn.pieces(text) {
                frames.push(Frame::content(ndjson(line(message(&piece, None), false))));
            }
        }
        Output::Tool { name, arguments } => {
            let message = message("", Some(("tool_calls", tool_calls(name, arguments))));
            frames.push(Frame::content(ndjson(line(message, false))));
        }
    }
    frames.push(Frame::meta(ndjson(line(message("", None), true))));
    Ok(Reply::Stream { content_type: "application/x-ndjson", frames })
}

/// `GET /api/tags`: the local models.
pub(crate) fn tags() -> Reply {
    let model = |name: &str, family: &str, parameters: &str| {
        json!({
            "name": name,
            "model": name,
            "modified_at": CREATED_AT,
            "size": 2_019_393_189u64,
            "digest": "0000000000000000000000000000000000000000000000000000000000000000",
            "details": {
                "parent_model": "",
                "format": "gguf",
                "family": family,
                "families": [family],
                "parameter_size": parameters,
                "quantization_level": "Q4_K_M",
            },
        })
    };
    Reply::Json(json!({
        "models": [
            model("llama3.2:latest", "llama", "3.2B"),
            model("qwen3:latest", "qwen3", "8.2B"),
        ],
    }))
}

/// `GET /api/version`.
pub(crate) fn version() -> Reply {
    Reply::Json(json!({ "version": "0.12.0" }))
}

/// `POST /api/show`: what a model can do and how long its context is.
pub(crate) fn show(call: &Call<'_>) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    let name = request.get("model").or_else(|| request.get("name")).and_then(Value::as_str).unwrap_or_default();
    let (family, capabilities) = match name.split(':').next().unwrap_or_default() {
        "qwen3" => ("qwen3", json!(["completion", "tools", "thinking"])),
        "llama3.2" => ("llama", json!(["completion", "tools"])),
        _ => return Err(ApiError::new(404, format!("model '{name}' not found"))),
    };
    Ok(Reply::Json(json!({
        "capabilities": capabilities,
        "details": { "family": family, "format": "gguf" },
        "model_info": { "general.architecture": family, format!("{family}.context_length"): 40_960 },
    })))
}
