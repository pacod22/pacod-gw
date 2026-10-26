//! Rerank (`POST /v1/rerank`): a query and documents in, the documents' order of relevance out.
//!
//! Callers speak Cohere's shape, which Jina, Voyage, vLLM and most SDKs follow: `query`,
//! `documents` (strings or `{text}`), `top_n`, `return_documents`. Providers differ in small
//! ways, so every call is written for its provider's shape ([`Shape`]) and its answer read back
//! into one form: Cohere's v2 API takes strings and returns no documents, Voyage names
//! `top_n` `top_k` and its results `data`.

use pacod_gw_events::Usage;
use serde_json::{Value, json};

use crate::error::DialectError;
use crate::util::{object, take_string, take_u64};

/// The most documents one request may hold, as Cohere allows.
const MAX_DOCUMENTS: usize = 1000;

/// The API a rerank provider speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// Cohere's `v2/rerank`.
    Cohere,
    /// Jina's `rerank`, which vLLM and Infinity serve as well.
    Jina,
    /// Voyage's `rerank`.
    Voyage,
}

/// A rerank request in neutral terms.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RerankRequest {
    pub model: String,
    pub query: String,
    pub documents: Vec<String>,
    pub top_n: Option<u64>,
    pub return_documents: bool,
    pub max_tokens_per_doc: Option<u64>,
}

impl RerankRequest {
    /// The characters of the query and the documents, for an estimate of the tokens.
    pub fn characters(&self) -> usize {
        self.query.chars().count() + self.documents.iter().map(|d| d.chars().count()).sum::<usize>()
    }
}

/// One ranked document.
#[derive(Debug, Clone, PartialEq)]
pub struct Ranked {
    pub index: u64,
    pub relevance_score: f64,
}

/// Reads a caller's request.
pub fn parse_request(body: &[u8]) -> Result<RerankRequest, DialectError> {
    let mut top = object(body)?;
    let query = take_string(&mut top, "query").filter(|q| !q.is_empty());
    let query = query.ok_or_else(|| DialectError::invalid("query is required: what the documents are ranked by"))?;
    let documents = match top.shift_remove("documents") {
        Some(Value::Array(list)) if !list.is_empty() => list
            .iter()
            .map(|d| match d {
                Value::String(s) => Ok(s.clone()),
                Value::Object(o) => o
                    .get("text")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| DialectError::invalid("a document object needs its text")),
                _ => Err(DialectError::invalid("a document is a string or an object with text")),
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err(DialectError::invalid("documents is required: a list of the texts to rank")),
    };
    if documents.len() > MAX_DOCUMENTS {
        return Err(DialectError::invalid(format!("at most {MAX_DOCUMENTS} documents are ranked at once")));
    }
    let top_n = take_u64(&mut top, "top_n").or_else(|| take_u64(&mut top, "top_k"));
    Ok(RerankRequest {
        model: take_string(&mut top, "model").unwrap_or_default(),
        query,
        documents,
        top_n,
        return_documents: top.get("return_documents").and_then(Value::as_bool).unwrap_or(false),
        max_tokens_per_doc: take_u64(&mut top, "max_tokens_per_doc"),
    })
}

/// The provider's request, for its model, and the path under its base URL.
pub fn build(req: &RerankRequest, shape: Shape, model: &str) -> (Value, &'static str) {
    let mut out = json!({ "model": model, "query": req.query, "documents": req.documents });
    match shape {
        Shape::Cohere => {
            if let Some(n) = req.top_n {
                out["top_n"] = json!(n);
            }
            if let Some(m) = req.max_tokens_per_doc {
                out["max_tokens_per_doc"] = json!(m);
            }
            (out, "v2/rerank")
        }
        Shape::Jina => {
            if let Some(n) = req.top_n {
                out["top_n"] = json!(n);
            }
            out["return_documents"] = json!(false);
            (out, "rerank")
        }
        Shape::Voyage => {
            if let Some(n) = req.top_n {
                out["top_k"] = json!(n);
            }
            out["return_documents"] = json!(false);
            (out, "rerank")
        }
    }
}

/// Reads a provider's answer: the ranking, best first, and what the provider billed.
pub fn read(body: &[u8], shape: Shape) -> Result<(Vec<Ranked>, Option<Usage>), DialectError> {
    let v: Value = serde_json::from_slice(body)
        .map_err(|_| DialectError::bad_response("the provider's answer is not JSON".to_owned()))?;
    let list = match shape {
        Shape::Voyage => v.get("data"),
        Shape::Cohere | Shape::Jina => v.get("results"),
    };
    let mut ranked: Vec<Ranked> = list
        .and_then(Value::as_array)
        .ok_or_else(|| DialectError::bad_response("the provider's answer has no results".to_owned()))?
        .iter()
        .filter_map(|r| {
            Some(Ranked {
                index: r.get("index")?.as_u64()?,
                relevance_score: r.get("relevance_score").and_then(Value::as_f64)?,
            })
        })
        .collect();
    ranked.sort_by(|a, b| b.relevance_score.total_cmp(&a.relevance_score).then(a.index.cmp(&b.index)));
    let usage = match shape {
        Shape::Cohere => v
            .pointer("/meta/billed_units/search_units")
            .and_then(Value::as_f64)
            .map(|n| Usage { search_units: n.ceil() as u64, ..Default::default() }),
        Shape::Jina | Shape::Voyage => v
            .pointer("/usage/total_tokens")
            .and_then(Value::as_u64)
            .map(|n| Usage { input_tokens: n, ..Default::default() }),
    };
    Ok((ranked, usage))
}

/// The caller's answer, in Cohere's shape, with the documents' text where it asked for it.
pub fn answer(id: &str, req: &RerankRequest, ranked: &[Ranked], usage: Option<&Usage>, cost: Option<&Value>) -> Value {
    let results: Vec<Value> = ranked
        .iter()
        .filter(|r| (r.index as usize) < req.documents.len())
        .take(req.top_n.map_or(usize::MAX, |n| n as usize))
        .map(|r| {
            let mut out = json!({ "index": r.index, "relevance_score": r.relevance_score });
            if req.return_documents {
                out["document"] = json!({ "text": req.documents[r.index as usize] });
            }
            out
        })
        .collect();
    let mut meta = json!({});
    if let Some(u) = usage.filter(|u| u.search_units > 0) {
        meta["billed_units"] = json!({ "search_units": u.search_units });
    }
    let mut out = json!({ "id": id, "results": results, "meta": meta });
    if let Some(u) = usage.filter(|u| u.input_tokens > 0) {
        out["usage"] = json!({ "total_tokens": u.input_tokens });
    }
    if let Some(c) = cost {
        out["meta"]["cost"] = c.clone();
    }
    out
}

/// The shape a provider's answer is in, read from its body: for proxy mode, which knows only
/// the path.
pub fn shape_of(body: &[u8]) -> Shape {
    let v: Value = serde_json::from_slice(body).unwrap_or(Value::Null);
    if v.get("meta").is_some() {
        Shape::Cohere
    } else if v.get("data").is_some() {
        Shape::Voyage
    } else {
        Shape::Jina
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> RerankRequest {
        parse_request(
            br#"{"model":"m","query":"capital of France","documents":["Berlin is in Germany",{"text":"Paris is the capital of France"},"Lyon"],"top_n":2,"return_documents":true}"#,
        )
        .unwrap()
    }

    #[test]
    fn a_request_is_read_in_cohere_shape() {
        let r = request();
        assert_eq!(r.documents, ["Berlin is in Germany", "Paris is the capital of France", "Lyon"]);
        assert_eq!((r.top_n, r.return_documents), (Some(2), true));
        assert!(parse_request(br#"{"query":"x","documents":[]}"#).is_err());
        assert!(parse_request(br#"{"query":"","documents":["a"]}"#).is_err());
        assert!(parse_request(br#"{"query":"x","documents":[1]}"#).is_err());
    }

    #[test]
    fn each_provider_is_asked_in_its_own_shape() {
        let r = request();
        let (cohere, path) = build(&r, Shape::Cohere, "rerank-v3.5");
        assert_eq!((path, cohere["top_n"].as_u64()), ("v2/rerank", Some(2)));
        assert!(cohere.get("return_documents").is_none(), "Cohere's v2 returns no documents");
        let (voyage, _) = build(&r, Shape::Voyage, "rerank-2");
        assert_eq!((voyage["top_k"].as_u64(), voyage.get("top_n")), (Some(2), None));
        let (jina, path) = build(&r, Shape::Jina, "jina-reranker-v2");
        assert_eq!((path, jina["documents"][1].as_str()), ("rerank", Some("Paris is the capital of France")));
    }

    #[test]
    fn answers_are_read_into_one_ranking_and_written_back() {
        let (ranked, usage) = read(
            br#"{"id":"x","results":[{"index":2,"relevance_score":0.1},{"index":1,"relevance_score":0.98}],"meta":{"billed_units":{"search_units":1}}}"#,
            Shape::Cohere,
        )
        .unwrap();
        assert_eq!(ranked[0].index, 1);
        assert_eq!(usage.unwrap().search_units, 1);
        let out = answer("rr_1", &request(), &ranked, usage.as_ref(), Some(&json!({ "amount": 0.002 })));
        assert_eq!(out["results"][0]["document"]["text"], "Paris is the capital of France");
        assert_eq!(out["meta"]["billed_units"]["search_units"], 1);
        assert_eq!(out["meta"]["cost"]["amount"], 0.002);
        let (ranked, usage) = read(
            br#"{"object":"list","data":[{"index":0,"relevance_score":0.5}],"usage":{"total_tokens":42}}"#,
            Shape::Voyage,
        )
        .unwrap();
        assert_eq!((ranked.len(), usage.unwrap().input_tokens), (1, 42));
        assert_eq!(shape_of(br#"{"data":[]}"#), Shape::Voyage);
        assert_eq!(shape_of(br#"{"results":[],"usage":{}}"#), Shape::Jina);
    }
}
