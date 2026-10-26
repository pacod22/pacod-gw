//! The Gemini native API: `generateContent`, `streamGenerateContent`, `countTokens`,
//! `embedContent`, `batchEmbedContents`, and Vertex's `predict` for its text embedding models.
//!
//! Shapes follow the public `GenerateContentResponse` reference and the recorded fixtures
//! (`fixtures/gemini/`). Where neither settles a detail, the comments say what was assumed.

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

/// Where the API is served.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Site {
    /// The plain mock's `/v1beta/models/…`, without Gemini 3's checks.
    Plain,
    /// AI Studio, `/emulate/gemini/v1beta/models/…`.
    Studio,
    /// Vertex AI, `/emulate/vertex/…`: reports `trafficType` and serves `predict`.
    Vertex,
}

/// `POST …/models/{model}:{method}`, routed by the method.
pub(crate) fn post(call: &Call<'_>, site: Site) -> Result<Reply, ApiError> {
    let not_found = || ApiError::new(404, format!("mock: no route for POST {}", call.path));
    let target = call.path.rfind("/models/").map(|i| &call.path[i + "/models/".len()..]).ok_or_else(not_found)?;
    let (model, action) = target.rsplit_once(':').ok_or_else(not_found)?;
    match action {
        "predict" if model.starts_with("imagen") => super::images::imagen(call),
        "generateContent" if super::images::wants_image(model, &call.json_object()?) => {
            super::images::gemini_image(call, &call.json_object()?)
        }
        "generateContent" => generate(call, model, false, site),
        "streamGenerateContent" => generate(call, model, true, site),
        "countTokens" => count_tokens(call),
        "embedContent" => embed(call),
        "batchEmbedContents" => batch_embed(call),
        "predict" if site == Site::Vertex => predict(call),
        _ => Err(not_found()),
    }
}

/// `generateContent` and `streamGenerateContent`. Away from the plain mock it adds Gemini 3's
/// checks: thought signatures on function calls of the current turn, no trailing model turn;
/// and every function call it answers carries a signature.
fn generate(call: &Call<'_>, model: &str, streaming: bool, site: Site) -> Result<Reply, ApiError> {
    let strict = site != Site::Plain;
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
            if site == Site::Vertex {
                // Vertex says how the call was served (its reference: `trafficType`).
                response["usageMetadata"]["trafficType"] = json!("ON_DEMAND");
            }
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

/// `countTokens`: the prompt's tokens, from `contents` or a whole `generateContentRequest`.
/// Recorded: `{"totalTokens":55,"promptTokensDetails":[{"modality":"TEXT","tokenCount":55}]}`.
fn count_tokens(call: &Call<'_>) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    let inner = request.get("generateContentRequest").filter(|v| v.is_object()).unwrap_or(&request);
    let mut text = String::new();
    parts_text(inner.get("systemInstruction").or_else(|| inner.get("system_instruction")), &mut text);
    for content in inner.get("contents").and_then(Value::as_array).into_iter().flatten() {
        parts_text(Some(content), &mut text);
    }
    let total = super::reply::count_tokens(&text).max(1);
    Ok(Reply::Json(
        json!({ "totalTokens": total, "promptTokensDetails": [{ "modality": "TEXT", "tokenCount": total }] }),
    ))
}

/// `embedContent`. Recorded: `{"embedding":{"values":[…]}}`, without usage.
fn embed(call: &Call<'_>) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    Ok(Reply::Json(json!({ "embedding": embedding_of(&request)? })))
}

/// `batchEmbedContents`: at most 100 requests, each naming its model. Recorded:
/// `{"embeddings":[{"values":[…]},…]}`, without usage.
fn batch_embed(call: &Call<'_>) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    let requests = request
        .get("requests")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("* BatchEmbedContentsRequest.requests: requests is required\n"))?;
    if requests.len() > 100 {
        return Err(ApiError::bad_request(
            "* BatchEmbedContentsRequest.requests: at most 100 requests can be in one batch\n",
        ));
    }
    let mut embeddings = Vec::with_capacity(requests.len());
    for item in requests {
        if !item.is_object() {
            return Err(ApiError::bad_request("a request is not an object"));
        }
        if !item.get("model").and_then(Value::as_str).is_some_and(|m| m.starts_with("models/")) {
            return Err(ApiError::bad_request("* BatchEmbedContentsRequest.requests[].model: model is required\n"));
        }
        embeddings.push(embedding_of(item)?);
    }
    Ok(Reply::Json(json!({ "embeddings": embeddings })))
}

/// Vertex `predict` for its text embedding models, as its reference describes the answer:
/// one prediction per instance, with a token count per instance.
fn predict(call: &Call<'_>) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    let dims = request.pointer("/parameters/outputDimensionality").and_then(Value::as_u64).unwrap_or(768);
    let instances = request
        .get("instances")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("instances is required"))?;
    let predictions: Vec<Value> = instances
        .iter()
        .map(|i| {
            let text = i.get("content").and_then(Value::as_str).unwrap_or("");
            json!({ "embeddings": { "values": vector(text, dims), "statistics": { "token_count": super::reply::count_tokens(text), "truncated": false } } })
        })
        .collect();
    let characters: usize = instances.iter().filter_map(|i| i.get("content")?.as_str()).map(str::len).sum();
    Ok(Reply::Json(json!({ "predictions": predictions, "metadata": { "billableCharacterCount": characters } })))
}

/// The `{"values":[…]}` of one embed request: `outputDimensionality` long (3072 when not
/// given, gemini-embedding-001's size), derived from the text.
fn embedding_of(request: &Value) -> Result<Value, ApiError> {
    let mut text = String::new();
    parts_text(request.get("content"), &mut text);
    if request.get("content").is_none() {
        return Err(ApiError::bad_request("* EmbedContentRequest.content: content is required\n"));
    }
    let dims = request
        .get("outputDimensionality")
        .or_else(|| request.pointer("/embedContentConfig/outputDimensionality"))
        .and_then(Value::as_u64)
        .unwrap_or(3072);
    Ok(json!({ "values": vector(&text, dims) }))
}

/// A deterministic vector of `dims` small numbers derived from `text`.
fn vector(text: &str, dims: u64) -> Vec<f64> {
    let mut state =
        text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
    (0..dims.min(8192))
        .map(|_| {
            state = state.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            ((state >> 40) as f64 / f64::from(1u32 << 24) - 0.5) / 10.0
        })
        .collect()
}

/// `GET /v1beta/models/{id}`: one entry of the list, in Gemini's shape.
pub(crate) fn model(id: &str) -> Reply {
    Reply::Json(model_entry(id))
}

fn model_entry(id: &str) -> Value {
    json!({
        "name": format!("models/{id}"),
        "version": "001",
        "displayName": id,
        "inputTokenLimit": 1_048_576,
        "outputTokenLimit": 65_536,
        "supportedGenerationMethods": ["generateContent", "countTokens"],
        "thinking": true,
    })
}

/// `GET /v1beta/models`: two chat models and an embedding model, which discovery leaves out.
pub(crate) fn models() -> Reply {
    let mut embedding = model_entry("gemini-embedding-2");
    embedding["supportedGenerationMethods"] = json!(["embedContent"]);
    Reply::Json(json!({ "models": [model_entry("gemini-3.8-flash"), model_entry("gemini-3.5-flash-lite"), embedding] }))
}
