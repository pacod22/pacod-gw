//! Image models: OpenAI's `/v1/images/generations` and `/v1/images/edits`, Imagen's
//! `:predict`, and Gemini's image models through `:generateContent`.
//!
//! The images are a few bytes that start as a PNG does, derived from the request, so that
//! tests can tell them apart and compare them. gpt-image reports usage in tokens; DALL·E and
//! Imagen report none, as their APIs do.

use axum::http::HeaderMap;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};

use super::{
    objects::form_parts,
    reply::{CREATED, count_tokens},
    server::{ApiError, Call, Reply},
};

/// An image, as base64: the PNG signature and eight bytes of the request's hash and index.
pub fn image(seed: u64, n: u64) -> String {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    bytes.extend_from_slice(&(seed ^ n).to_be_bytes());
    STANDARD.encode(bytes)
}

/// gpt-image's tokens for an image of `size`: what OpenAI documents for its medium quality,
/// rounded (1024×1024 is 1056 tokens).
fn image_tokens(size: &str) -> u64 {
    match size {
        "1536x1024" | "1024x1536" => 1584,
        _ => 1056,
    }
}

fn answer(call: &Call<'_>, model: &str, prompt: &str, n: u64, size: &str, input_images: u64, url: bool) -> Value {
    let data: Vec<Value> = (0..n)
        .map(|i| {
            if url {
                json!({ "url": format!("https://images.mock.invalid/{:016x}-{i}.png", call.seed) })
            } else {
                json!({ "b64_json": image(call.seed, i), "revised_prompt": prompt })
            }
        })
        .collect();
    let mut out = json!({ "created": CREATED, "data": data });
    if model.starts_with("gpt-image") && !call.behavior.no_usage {
        let text = count_tokens(prompt);
        let images = input_images * 323;
        let output = n * image_tokens(size);
        out["usage"] = json!({
            "input_tokens": text + images,
            "input_tokens_details": { "text_tokens": text, "image_tokens": images },
            "output_tokens": output,
            "total_tokens": text + images + output,
        });
        out["output_format"] = json!("png");
    }
    out
}

/// `POST /v1/images/generations`.
pub(crate) fn generations(call: &Call<'_>) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    let model = request.get("model").and_then(Value::as_str).unwrap_or("dall-e-2");
    let prompt = request
        .get("prompt")
        .and_then(Value::as_str)
        .ok_or_else(|| ApiError::bad_request("Missing required parameter: 'prompt'."))?;
    let n = request.get("n").and_then(Value::as_u64).unwrap_or(1);
    let size = request.get("size").and_then(Value::as_str).unwrap_or("1024x1024");
    let url =
        !model.starts_with("gpt-image") && request.get("response_format").and_then(Value::as_str) != Some("b64_json");
    Ok(Reply::Json(answer(call, model, prompt, n, size, 0, url)))
}

/// `POST /v1/images/edits`: a form with `image` (or `image[]`), `prompt` and `model`.
pub(crate) fn edits(call: &Call<'_>, headers: &HeaderMap) -> Result<Reply, ApiError> {
    let parts = form_parts(headers, call.body)?;
    let text =
        |name: &str| parts.iter().find(|p| p.name == name).map(|p| String::from_utf8_lossy(&p.body).trim().to_owned());
    let images = parts.iter().filter(|p| p.name == "image" || p.name == "image[]").count() as u64;
    if images == 0 {
        return Err(ApiError::bad_request("Missing required parameter: 'image'."));
    }
    let prompt = text("prompt").ok_or_else(|| ApiError::bad_request("Missing required parameter: 'prompt'."))?;
    let model = text("model").unwrap_or_else(|| "dall-e-2".into());
    let n = text("n").and_then(|n| n.parse().ok()).unwrap_or(1);
    let size = text("size").unwrap_or_else(|| "1024x1024".into());
    let url = !model.starts_with("gpt-image") && text("response_format").as_deref() != Some("b64_json");
    Ok(Reply::Json(answer(call, &model, &prompt, n, &size, images, url)))
}

/// Imagen's `:predict`: one prediction per sample, no usage.
pub(crate) fn imagen(call: &Call<'_>) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    if request.pointer("/instances/0/prompt").and_then(Value::as_str).is_none_or(str::is_empty) {
        return Err(ApiError::bad_request("instances[0].prompt is required"));
    }
    let n = request.pointer("/parameters/sampleCount").and_then(Value::as_u64).unwrap_or(1);
    let mime = request.pointer("/parameters/outputOptions/mimeType").and_then(Value::as_str).unwrap_or("image/png");
    let predictions: Vec<Value> =
        (0..n).map(|i| json!({ "bytesBase64Encoded": image(call.seed, i), "mimeType": mime })).collect();
    Ok(Reply::Json(json!({ "predictions": predictions })))
}

/// Whether a `generateContent` request asks a Gemini image model for images.
pub(crate) fn wants_image(model: &str, request: &Value) -> bool {
    let modalities = request.pointer("/generationConfig/responseModalities").and_then(Value::as_array);
    model.contains("-image") && modalities.is_some_and(|m| m.iter().any(|x| x == "IMAGE"))
}

/// A Gemini image model's answer: a line of text and one image, with usage by modality.
pub(crate) fn gemini_image(call: &Call<'_>, request: &Value) -> Result<Reply, ApiError> {
    let prompt: String = request
        .get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|c| c.get("parts").and_then(Value::as_array).into_iter().flatten())
        .filter_map(|p| p.get("text").and_then(Value::as_str))
        .collect();
    let inputs = request
        .get("contents")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .flat_map(|c| c.get("parts").and_then(Value::as_array).into_iter().flatten())
        .filter(|p| p.get("inlineData").is_some())
        .count() as u64;
    let text = count_tokens(&prompt);
    Ok(Reply::Json(json!({
        "candidates": [{
            "content": { "role": "model", "parts": [
                { "text": "Here is the image." },
                { "inlineData": { "mimeType": "image/png", "data": image(call.seed, 0) } }
            ] },
            "finishReason": "STOP",
            "index": 0
        }],
        "usageMetadata": {
            "promptTokenCount": text + inputs * 258,
            "candidatesTokenCount": 1295,
            "totalTokenCount": text + inputs * 258 + 1295,
            "promptTokensDetails": [{ "modality": "TEXT", "tokenCount": text }, { "modality": "IMAGE", "tokenCount": inputs * 258 }],
            "candidatesTokensDetails": [{ "modality": "TEXT", "tokenCount": 5 }, { "modality": "IMAGE", "tokenCount": 1290 }]
        },
        "modelVersion": "gemini-image-mock",
        "responseId": format!("mock{:016x}", call.seed)
    })))
}
