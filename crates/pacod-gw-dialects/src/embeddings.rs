//! Embeddings across dialects, and counting a Gemini model's tokens for Anthropic callers.
//!
//! The gateway's embeddings endpoint speaks OpenAI's `POST /v1/embeddings`. To an OpenAI-shaped
//! provider the request goes as it is; for Gemini's own API it becomes `batchEmbedContents`
//! (at most 100 inputs a call, recorded 2026-10-11), and for Vertex AI `predict`. The vectors
//! come back as OpenAI writes them: numbers, or base64 of little-endian float32 when the caller
//! asks for `encoding_format: base64` (the OpenAI SDKs' default).
//!
//! Neither Gemini call reports token usage (recorded); Vertex's `predict` counts tokens per
//! input, by its reference.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value, json};

use crate::DialectError;

/// The most inputs one embeddings request may hold (OpenAI's own limit).
pub const MAX_INPUTS: usize = 2048;
/// The most inputs Gemini's `batchEmbedContents` takes in one call (recorded).
pub const GEMINI_BATCH: usize = 100;

/// An embeddings request in OpenAI's shape, read.
#[derive(Debug, Clone, PartialEq)]
pub struct EmbedRequest {
    pub inputs: Vec<String>,
    /// `dimensions`: the length of each vector, where the model can shorten it.
    pub dimensions: Option<u64>,
    /// `encoding_format: base64`.
    pub base64: bool,
    /// `provider_options.gemini` (`taskType`, `title`, …), merged into each Gemini request.
    pub gemini_options: Map<String, Value>,
}

/// Reads `{"model", "input", "dimensions", "encoding_format", …}`. Token arrays are refused:
/// only OpenAI's own models read them.
pub fn parse_request(body: &[u8]) -> Result<EmbedRequest, DialectError> {
    let v: Value =
        serde_json::from_slice(body).map_err(|e| DialectError::invalid(format!("the body is not JSON: {e}")))?;
    let inputs: Vec<String> = match v.get("input") {
        Some(Value::String(s)) => vec![s.clone()],
        Some(Value::Array(items)) if items.iter().all(Value::is_string) => {
            items.iter().filter_map(Value::as_str).map(str::to_owned).collect()
        }
        Some(Value::Array(_)) => {
            return Err(DialectError::invalid(
                "input: token arrays are read by OpenAI's models only; send the text instead",
            ));
        }
        _ => return Err(DialectError::invalid("input is required: a text or a list of texts")),
    };
    if inputs.is_empty() || inputs.len() > MAX_INPUTS {
        return Err(DialectError::invalid(format!("input holds between 1 and {MAX_INPUTS} texts")));
    }
    let base64 = match v.get("encoding_format").and_then(Value::as_str) {
        None | Some("float") => false,
        Some("base64") => true,
        Some(other) => {
            return Err(DialectError::invalid(format!("encoding_format \"{other}\" is not float or base64")));
        }
    };
    let gemini_options = v.pointer("/provider_options/gemini").and_then(Value::as_object).cloned().unwrap_or_default();
    Ok(EmbedRequest { inputs, dimensions: v.get("dimensions").and_then(Value::as_u64), base64, gemini_options })
}

/// The bodies for Gemini's `batchEmbedContents`, in calls of at most 100 inputs.
pub fn gemini_batches(model: &str, request: &EmbedRequest) -> Vec<Value> {
    request
        .inputs
        .chunks(GEMINI_BATCH)
        .map(|chunk| {
            let requests: Vec<Value> = chunk
                .iter()
                .map(|text| {
                    let mut r =
                        json!({ "model": format!("models/{model}"), "content": { "parts": [{ "text": text }] } });
                    if let Some(d) = request.dimensions {
                        r["outputDimensionality"] = json!(d);
                    }
                    for (k, v) in &request.gemini_options {
                        r[k] = v.clone();
                    }
                    r
                })
                .collect();
            json!({ "requests": requests })
        })
        .collect()
}

/// The bodies for Vertex AI's `predict`: Gemini's embedding models take one text a call, the
/// others up to 250 (Vertex AI's reference; not recorded).
pub fn vertex_predictions(model: &str, request: &EmbedRequest) -> Vec<Value> {
    let per_call = if model.contains("gemini-embedding") { 1 } else { 250 };
    request
        .inputs
        .chunks(per_call)
        .map(|chunk| {
            let instances: Vec<Value> = chunk
                .iter()
                .map(|text| {
                    let mut i = json!({ "content": text });
                    if let Some(t) = request.gemini_options.get("taskType") {
                        i["task_type"] = t.clone();
                    }
                    i
                })
                .collect();
            let mut body = json!({ "instances": instances });
            if let Some(d) = request.dimensions {
                body["parameters"] = json!({ "outputDimensionality": d });
            }
            body
        })
        .collect()
}

/// The vectors of a `batchEmbedContents` (or `embedContent`) answer.
pub fn read_gemini(body: &[u8]) -> Result<Vec<Vec<f32>>, DialectError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| DialectError::bad_response(format!("not JSON: {e}")))?;
    let list: Vec<&Value> = match (v.get("embeddings"), v.get("embedding")) {
        (Some(Value::Array(items)), _) => items.iter().collect(),
        (_, Some(one)) => vec![one],
        _ => return Err(DialectError::bad_response("the answer holds no embeddings")),
    };
    list.into_iter().map(values).collect()
}

/// The vectors and token count of a Vertex AI `predict` answer.
pub fn read_vertex(body: &[u8]) -> Result<(Vec<Vec<f32>>, u64), DialectError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| DialectError::bad_response(format!("not JSON: {e}")))?;
    let predictions = v["predictions"].as_array().ok_or_else(|| DialectError::bad_response("no predictions"))?;
    let mut vectors = Vec::with_capacity(predictions.len());
    let mut tokens = 0;
    for p in predictions {
        vectors.push(values(&p["embeddings"])?);
        tokens += p.pointer("/embeddings/statistics/token_count").and_then(Value::as_f64).unwrap_or(0.0) as u64;
    }
    Ok((vectors, tokens))
}

fn values(embedding: &Value) -> Result<Vec<f32>, DialectError> {
    embedding["values"]
        .as_array()
        .ok_or_else(|| DialectError::bad_response("an embedding without values"))?
        .iter()
        .map(|x| x.as_f64().map(|f| f as f32).ok_or_else(|| DialectError::bad_response("a value that is not a number")))
        .collect()
}

/// OpenAI's answer: `{"object":"list","data":[{"object":"embedding","index","embedding"}],
/// "model","usage":{"prompt_tokens","total_tokens"}}`.
pub fn openai_answer(model: &str, vectors: &[Vec<f32>], base64: bool, prompt_tokens: u64) -> Value {
    let data: Vec<Value> = vectors
        .iter()
        .enumerate()
        .map(|(index, vector)| {
            let embedding = if base64 {
                let bytes: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
                json!(STANDARD.encode(bytes))
            } else {
                json!(vector)
            };
            json!({ "object": "embedding", "index": index, "embedding": embedding })
        })
        .collect();
    json!({ "object": "list", "data": data, "model": model,
            "usage": { "prompt_tokens": prompt_tokens, "total_tokens": prompt_tokens } })
}

/// The body of Gemini's `countTokens` for a request built for `generateContent`: AI Studio
/// takes it whole under `generateContentRequest` (recorded), Vertex AI its members flat (its
/// reference; not recorded).
pub fn gemini_count_body(generate: &Value, model: &str, vertex: bool) -> Value {
    let mut inner = Map::new();
    for key in ["contents", "systemInstruction", "tools", "toolConfig"] {
        if let Some(v) = generate.get(key) {
            inner.insert(key.into(), v.clone());
        }
    }
    if vertex {
        Value::Object(inner)
    } else {
        inner.insert("model".into(), json!(format!("models/{model}")));
        json!({ "generateContentRequest": inner })
    }
}

/// Anthropic's answer to `count_tokens` from Gemini's `{"totalTokens": n}`.
pub fn anthropic_count_answer(body: &[u8]) -> Result<Value, DialectError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| DialectError::bad_response(format!("not JSON: {e}")))?;
    let total = v["totalTokens"].as_u64().ok_or_else(|| DialectError::bad_response("no totalTokens in the answer"))?;
    Ok(json!({ "input_tokens": total }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_become_gemini_batches_of_a_hundred() {
        let texts: Vec<String> = (0..250).map(|i| format!("t{i}")).collect();
        let body = json!({ "model": "m", "input": texts, "dimensions": 8, "provider_options": { "gemini": { "taskType": "RETRIEVAL_QUERY" } } });
        let r = parse_request(body.to_string().as_bytes()).unwrap();
        let batches = gemini_batches("gemini-embedding-001", &r);
        assert_eq!(batches.iter().map(|b| b["requests"].as_array().unwrap().len()).collect::<Vec<_>>(), [100, 100, 50]);
        let first = &batches[0]["requests"][0];
        assert_eq!(first["model"], "models/gemini-embedding-001");
        assert_eq!(
            (first["outputDimensionality"].as_u64(), first["taskType"].as_str()),
            (Some(8), Some("RETRIEVAL_QUERY"))
        );
        // One text, and the errors.
        assert_eq!(parse_request(br#"{"input":"one"}"#).unwrap().inputs, ["one"]);
        assert!(parse_request(br#"{"input":[[1,2,3]]}"#).unwrap_err().to_string().contains("token arrays"));
        assert!(parse_request(br#"{"input":[]}"#).is_err());
        assert!(parse_request(br#"{"input":"x","encoding_format":"hex"}"#).is_err());
    }

    #[test]
    fn vectors_come_back_as_openai_writes_them() {
        let answer = br#"{"embeddings":[{"values":[0.5,-1.0]},{"values":[0.25,2.0]}]}"#;
        let vectors = read_gemini(answer).unwrap();
        let floats = openai_answer("gateway/embed", &vectors, false, 7);
        assert_eq!(floats["data"][1]["embedding"], json!([0.25, 2.0]));
        assert_eq!(
            (floats["data"][1]["index"].as_u64(), floats["usage"]["prompt_tokens"].as_u64()),
            (Some(1), Some(7))
        );
        let b64 = openai_answer("gateway/embed", &vectors, true, 7);
        let bytes = STANDARD.decode(b64["data"][0]["embedding"].as_str().unwrap()).unwrap();
        let back: Vec<f32> = bytes.chunks(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        assert_eq!(back, [0.5, -1.0], "little-endian float32, as OpenAI encodes them");
        assert_eq!(read_gemini(br#"{"embedding":{"values":[1.0]}}"#).unwrap(), [[1.0]]);
        let (v, tokens) = read_vertex(br#"{"predictions":[{"embeddings":{"values":[1.0],"statistics":{"token_count":4}}},{"embeddings":{"values":[2.0],"statistics":{"token_count":3}}}]}"#).unwrap();
        assert_eq!((v.len(), tokens), (2, 7));
    }

    #[test]
    fn token_counts_are_asked_and_answered() {
        let generate = json!({ "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }], "systemInstruction": { "parts": [] }, "generationConfig": { "maxOutputTokens": 5 } });
        let studio = gemini_count_body(&generate, "gemini-3.8-flash", false);
        assert_eq!(studio["generateContentRequest"]["model"], "models/gemini-3.8-flash");
        assert!(studio["generateContentRequest"].get("generationConfig").is_none());
        let vertex = gemini_count_body(&generate, "gemini-3.8-flash", true);
        assert!(vertex.get("contents").is_some() && vertex.get("model").is_none());
        assert_eq!(anthropic_count_answer(br#"{"totalTokens":55}"#).unwrap(), json!({ "input_tokens": 55 }));
    }
}
