//! Rerank in three providers' shapes, chosen by the path: Cohere's `…/v2/rerank`, Voyage's
//! under a path that names `voyage`, and Jina's (which vLLM serves too) at any other
//! `…/rerank`.
//!
//! A document's score is the share of the query's words it contains, so tests can predict the
//! order. Cohere bills one search unit per 100 documents; Jina and Voyage report tokens.

use serde_json::{Value, json};

use super::{
    reply::count_tokens,
    server::{ApiError, Call, Reply},
};

fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric()).filter(|w| !w.is_empty()).map(str::to_lowercase).collect()
}

/// `POST …/rerank`.
pub(crate) fn rerank(call: &Call<'_>) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    let query =
        request.get("query").and_then(Value::as_str).ok_or_else(|| ApiError::bad_request("query is required"))?;
    let documents: Vec<String> = request
        .get("documents")
        .and_then(Value::as_array)
        .ok_or_else(|| ApiError::bad_request("documents is required"))?
        .iter()
        .map(|d| d.as_str().map(str::to_owned).or_else(|| d.get("text").and_then(Value::as_str).map(str::to_owned)))
        .collect::<Option<_>>()
        .ok_or_else(|| ApiError::bad_request("documents must be strings"))?;
    let asked = words(query);
    let mut scored: Vec<(usize, f64)> = documents
        .iter()
        .enumerate()
        .map(|(i, d)| {
            let have = words(d);
            let hits = asked.iter().filter(|w| have.contains(w)).count();
            (i, if asked.is_empty() { 0.0 } else { hits as f64 / asked.len() as f64 })
        })
        .collect();
    scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    let voyage = call.path.contains("voyage");
    let limit =
        request.get(if voyage { "top_k" } else { "top_n" }).and_then(Value::as_u64).map_or(usize::MAX, |n| n as usize);
    let results: Vec<Value> =
        scored.iter().take(limit).map(|(i, s)| json!({ "index": i, "relevance_score": s })).collect();
    let tokens = count_tokens(query) + documents.iter().map(|d| count_tokens(d)).sum::<u64>();
    let model = request.get("model").and_then(Value::as_str).unwrap_or("rerank");
    Ok(Reply::Json(if call.path.ends_with("/v2/rerank") {
        json!({ "id": format!("mock{:016x}", call.seed), "results": results,
                "meta": { "api_version": { "version": "2" }, "billed_units": { "search_units": documents.len().div_ceil(100) } } })
    } else if voyage {
        json!({ "object": "list", "data": results, "model": model, "usage": { "total_tokens": tokens } })
    } else {
        json!({ "model": model, "usage": { "total_tokens": tokens }, "results": results })
    }))
}
