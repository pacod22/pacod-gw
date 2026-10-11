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
    /// A Gemini caller's options per input (`taskType`, `title`), in the order of `inputs`;
    /// empty when they are the same for all.
    pub item_options: Vec<Map<String, Value>>,
}

impl EmbedRequest {
    /// Gemini's options for one input: the request's, then the input's own.
    fn options(&self, i: usize) -> Map<String, Value> {
        let mut o = self.gemini_options.clone();
        for (k, v) in self.item_options.get(i).into_iter().flatten() {
            o.insert(k.clone(), v.clone());
        }
        o
    }
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
    Ok(EmbedRequest {
        inputs,
        dimensions: v.get("dimensions").and_then(Value::as_u64),
        base64,
        gemini_options,
        item_options: Vec::new(),
    })
}

/// The three ways a Gemini caller asks for embeddings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiEmbed {
    /// `:embedContent`: one content (Gemini's API, and Vertex AI for Gemini models).
    One,
    /// `:batchEmbedContents`: a list of requests (Gemini's API).
    Batch,
    /// `:predict`: a list of instances (Vertex AI).
    Predict,
}

/// A member under its camelCase name or its snake_case one: Google's JSON parser takes both.
fn member<'a>(v: &'a Value, camel: &str, snake: &str) -> Option<&'a Value> {
    v.get(camel).or_else(|| v.get(snake))
}

/// The text of a content to embed. Its text parts are one text, as Gemini reads them (the
/// SDKs put a list of strings for `embedContent` into one content).
fn content_text(content: Option<&Value>) -> Result<String, DialectError> {
    let parts = content.and_then(|c| c.get("parts")).and_then(Value::as_array).filter(|p| !p.is_empty());
    let Some(parts) = parts else {
        return Err(DialectError::invalid("each content to embed holds text parts"));
    };
    parts
        .iter()
        .map(|p| {
            p.get("text")
                .and_then(Value::as_str)
                .ok_or_else(|| DialectError::unsupported("only text is embedded through the gateway"))
        })
        .collect()
}

/// `taskType` and `title`, wherever the caller put them.
fn gemini_item_options(v: &Value) -> Map<String, Value> {
    let mut o = Map::new();
    for (camel, snake) in [("taskType", "task_type"), ("title", "title")] {
        if let Some(x) = member(v, camel, snake) {
            o.insert(camel.into(), x.clone());
        }
    }
    o
}

/// Reads a Gemini caller's embeddings request; also the models its batch's requests name, for
/// the caller to check against the model of the path.
pub fn parse_gemini(body: &[u8], shape: GeminiEmbed) -> Result<(EmbedRequest, Vec<String>), DialectError> {
    let v: Value =
        serde_json::from_slice(body).map_err(|e| DialectError::invalid(format!("the body is not JSON: {e}")))?;
    let dims = |x: &Value| member(x, "outputDimensionality", "output_dimensionality").and_then(Value::as_u64);
    let mut request = EmbedRequest {
        inputs: Vec::new(),
        dimensions: None,
        base64: false,
        gemini_options: Map::new(),
        item_options: Vec::new(),
    };
    let mut models = Vec::new();
    match shape {
        GeminiEmbed::One => {
            // Vertex AI's form keeps the options in `embedContentConfig`.
            let config = member(&v, "embedContentConfig", "embed_content_config").unwrap_or(&v);
            request.inputs.push(content_text(v.get("content"))?);
            request.dimensions = dims(config).or_else(|| dims(&v));
            request.item_options.push(gemini_item_options(config));
        }
        GeminiEmbed::Batch => {
            let items = v.get("requests").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
            for item in items {
                request.inputs.push(content_text(item.get("content"))?);
                request.item_options.push(gemini_item_options(item));
                if let Some(m) = item.get("model").and_then(Value::as_str) {
                    models.push(m.to_owned());
                }
                let d = dims(item);
                if request.inputs.len() > 1 && d != request.dimensions {
                    return Err(DialectError::invalid("outputDimensionality must be the same for every request"));
                }
                request.dimensions = d;
            }
        }
        GeminiEmbed::Predict => {
            let items = v.get("instances").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
            for item in items {
                let text = match item.get("content") {
                    Some(Value::String(s)) => s.clone(),
                    other => content_text(other)?,
                };
                request.inputs.push(text);
                request.item_options.push(gemini_item_options(item));
            }
            request.dimensions = v.get("parameters").and_then(dims);
        }
    }
    if request.inputs.is_empty() || request.inputs.len() > MAX_INPUTS {
        return Err(DialectError::invalid(format!("between 1 and {MAX_INPUTS} contents are embedded at a time")));
    }
    Ok((request, models))
}

/// The answer a Gemini caller expects, in the shape it asked in. `counts` are the tokens of each
/// input where the provider reported them (Vertex AI's `predict`).
pub fn gemini_answer(shape: GeminiEmbed, vectors: &[Vec<f32>], counts: &[u64]) -> Value {
    let values = |v: &Vec<f32>| json!({ "values": v });
    match shape {
        GeminiEmbed::One => json!({ "embedding": vectors.first().map(values) }),
        GeminiEmbed::Batch => json!({ "embeddings": vectors.iter().map(values).collect::<Vec<_>>() }),
        GeminiEmbed::Predict => {
            let predictions: Vec<Value> = vectors
                .iter()
                .enumerate()
                .map(|(i, v)| {
                    let mut e = values(v);
                    if let Some(n) = counts.get(i) {
                        e["statistics"] = json!({ "token_count": n, "truncated": false });
                    }
                    json!({ "embeddings": e })
                })
                .collect();
            json!({ "predictions": predictions })
        }
    }
}

/// The body for an OpenAI-shaped provider's `/embeddings`, as numbers.
pub fn openai_request(model: &str, request: &EmbedRequest) -> Value {
    let mut body = json!({ "model": model, "input": request.inputs, "encoding_format": "float" });
    if let Some(d) = request.dimensions {
        body["dimensions"] = json!(d);
    }
    body
}

/// The vectors and input tokens of an OpenAI-shaped answer, in the order of the inputs.
pub fn read_openai(body: &[u8]) -> Result<(Vec<Vec<f32>>, Option<u64>), DialectError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| DialectError::bad_response(format!("not JSON: {e}")))?;
    let mut data: Vec<&Value> =
        v["data"].as_array().ok_or_else(|| DialectError::bad_response("no data"))?.iter().collect();
    data.sort_by_key(|d| d["index"].as_u64().unwrap_or(0));
    let vectors = data
        .into_iter()
        .map(|d| {
            d["embedding"]
                .as_array()
                .ok_or_else(|| DialectError::bad_response("an embedding that is not a list of numbers"))?
                .iter()
                .map(|x| {
                    x.as_f64()
                        .map(|f| f as f32)
                        .ok_or_else(|| DialectError::bad_response("a value that is not a number"))
                })
                .collect()
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((vectors, v.pointer("/usage/prompt_tokens").and_then(Value::as_u64)))
}

/// The bodies for Gemini's `batchEmbedContents`, in calls of at most 100 inputs.
pub fn gemini_batches(model: &str, request: &EmbedRequest) -> Vec<Value> {
    let numbered: Vec<(usize, &String)> = request.inputs.iter().enumerate().collect();
    numbered
        .chunks(GEMINI_BATCH)
        .map(|chunk| {
            let requests: Vec<Value> = chunk
                .iter()
                .copied()
                .map(|(i, text)| {
                    let mut r =
                        json!({ "model": format!("models/{model}"), "content": { "parts": [{ "text": text }] } });
                    if let Some(d) = request.dimensions {
                        r["outputDimensionality"] = json!(d);
                    }
                    for (k, v) in request.options(i) {
                        r[k] = v;
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
    let numbered: Vec<(usize, &String)> = request.inputs.iter().enumerate().collect();
    numbered
        .chunks(per_call)
        .map(|chunk| {
            let instances: Vec<Value> = chunk
                .iter()
                .map(|(n, text)| {
                    let mut i = json!({ "content": text });
                    let options = request.options(*n);
                    if let Some(t) = options.get("taskType") {
                        i["task_type"] = t.clone();
                    }
                    if let Some(t) = options.get("title") {
                        i["title"] = t.clone();
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

/// The vectors of a Vertex AI `predict` answer, and the tokens of each input.
pub fn read_vertex(body: &[u8]) -> Result<(Vec<Vec<f32>>, Vec<u64>), DialectError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| DialectError::bad_response(format!("not JSON: {e}")))?;
    let predictions = v["predictions"].as_array().ok_or_else(|| DialectError::bad_response("no predictions"))?;
    let mut vectors = Vec::with_capacity(predictions.len());
    let mut counts = Vec::with_capacity(predictions.len());
    for p in predictions {
        vectors.push(values(&p["embeddings"])?);
        counts.push(p.pointer("/embeddings/statistics/token_count").and_then(Value::as_f64).unwrap_or(0.0) as u64);
    }
    Ok((vectors, counts))
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

/// Anthropic's `count_tokens` body from a Messages request the gateway wrote: the members that
/// endpoint takes.
pub fn anthropic_count_body(messages_request: &Value) -> Value {
    let mut out = Map::new();
    for key in ["model", "messages", "system", "tools", "tool_choice", "thinking"] {
        if let Some(v) = messages_request.get(key) {
            out.insert(key.into(), v.clone());
        }
    }
    Value::Object(out)
}

/// Gemini's answer to `countTokens` from Anthropic's `{"input_tokens": n}`.
pub fn gemini_count_answer(body: &[u8]) -> Result<Value, DialectError> {
    let v: Value = serde_json::from_slice(body).map_err(|e| DialectError::bad_response(format!("not JSON: {e}")))?;
    let total =
        v["input_tokens"].as_u64().ok_or_else(|| DialectError::bad_response("no input_tokens in the answer"))?;
    Ok(json!({ "totalTokens": total }))
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
        let (v, counts) = read_vertex(br#"{"predictions":[{"embeddings":{"values":[1.0],"statistics":{"token_count":4}}},{"embeddings":{"values":[2.0],"statistics":{"token_count":3}}}]}"#).unwrap();
        assert_eq!((v.len(), counts), (2, vec![4, 3]));
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

    #[test]
    fn gemini_callers_ask_in_three_shapes() {
        // Vertex AI's embedContent: options in embedContentConfig, a content's text parts as one text.
        let (one, _) = parse_gemini(
            br#"{"content":{"parts":[{"text":"The quick "},{"text":"brown fox."}]},"embedContentConfig":{"taskType":"RETRIEVAL_QUERY","outputDimensionality":8}}"#,
            GeminiEmbed::One,
        )
        .unwrap();
        assert_eq!((one.inputs.as_slice(), one.dimensions), (&["The quick brown fox.".to_owned()][..], Some(8)));
        assert_eq!(one.item_options[0]["taskType"], "RETRIEVAL_QUERY");
        // A batch: each request's model and options.
        let (batch, models) = parse_gemini(
            br#"{"requests":[{"model":"models/m","content":{"parts":[{"text":"a"}]},"task_type":"CLUSTERING"},{"model":"models/m","content":{"parts":[{"text":"b"}]}}]}"#,
            GeminiEmbed::Batch,
        )
        .unwrap();
        assert_eq!((batch.inputs.len(), models), (2, vec!["models/m".to_owned(); 2]));
        assert_eq!(gemini_batches("x", &batch)[0]["requests"][0]["taskType"], "CLUSTERING");
        let differ = br#"{"requests":[{"content":{"parts":[{"text":"a"}]},"outputDimensionality":8},{"content":{"parts":[{"text":"b"}]}}]}"#;
        assert!(parse_gemini(differ, GeminiEmbed::Batch).is_err());
        // Vertex AI's predict.
        let (predict, _) = parse_gemini(
            br#"{"instances":[{"content":"a","task_type":"RETRIEVAL_DOCUMENT","title":"t"}],"parameters":{"outputDimensionality":4}}"#,
            GeminiEmbed::Predict,
        )
        .unwrap();
        assert_eq!(vertex_predictions("text-embedding-005", &predict)[0]["instances"][0]["title"], "t");
        assert_eq!(predict.dimensions, Some(4));
        // Only text.
        let image = br#"{"content":{"parts":[{"inlineData":{"mimeType":"image/png","data":"AA"}}]}}"#;
        assert!(parse_gemini(image, GeminiEmbed::One).unwrap_err().to_string().contains("only text"));
        // The answers, in the shape asked.
        let v = vec![vec![0.5f32]];
        assert_eq!(gemini_answer(GeminiEmbed::One, &v, &[]), json!({ "embedding": { "values": [0.5] } }));
        assert_eq!(
            gemini_answer(GeminiEmbed::Predict, &v, &[3])["predictions"][0]["embeddings"]["statistics"]["token_count"],
            3
        );
    }
}
