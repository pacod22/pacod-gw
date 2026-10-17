//! TypeSafe's System One API (`POST /v1/systemone`), for the gateway's Jev engine
//! (`docs/plugin-concept.md`, section 5.5). A probe's `GET /v1/models` is answered as OpenAI's.
//!
//! The answers are chosen by keyword so that tests can steer them, in the shapes TypeSafe
//! documents: a choice is the first option whose words appear in the text (`legal_advice`
//! matches "legal advice"), else `unknown` where it is an option, else the first; it has
//! probability 0.9 and confidence 0.9 when found by keyword, 0.4 otherwise. A `noul` is 0.95
//! when the text names the question's id, 0.05 otherwise. A score is its lowest level, 0.

use serde_json::{Map, Value, json};

use super::reply::count_tokens;
use super::server::{ApiError, Call, Reply};

/// `POST /v1/systemone`.
pub(crate) fn systemone(call: &Call<'_>) -> Result<Reply, ApiError> {
    let request = call.json_object()?;
    let model = request.get("model").and_then(Value::as_str).ok_or_else(|| ApiError::new(422, "model is required"))?;
    let questions = request
        .get("questions")
        .and_then(Value::as_object)
        .ok_or_else(|| ApiError::new(422, "questions is required"))?;
    let state = request.get("state").ok_or_else(|| ApiError::new(422, "state is required"))?;
    let text = match state {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
    .to_lowercase();

    let mut answers = Map::new();
    for (id, q) in questions {
        let answer = match q.get("type").and_then(Value::as_str) {
            Some("choice") => choice(q, &text),
            Some("noul") => {
                json!({ "type": "noul", "noul": if text.contains(&id.replace('_', " ")) { 0.95 } else { 0.05 } })
            }
            Some("score") => {
                let levels = q.get("criteria").and_then(Value::as_array).map_or(0, Vec::len);
                if !(2..=10).contains(&levels) {
                    return Err(ApiError::new(422, format!("questions.{id}.criteria: a score has 2 to 10 levels")));
                }
                let legend: Map<String, Value> = (0..levels)
                    .map(|n| (format!("{}", n as f64 / (levels - 1) as f64), q["criteria"][n].clone()))
                    .collect();
                json!({ "type": "score", "score": 0.0, "confidence": 1.0, "legend": legend, "probabilities": {} })
            }
            _ => {
                return Err(ApiError::new(
                    422,
                    format!("questions.{id}.type: expected one of 'noul', 'choice', 'score'"),
                ));
            }
        };
        answers.insert(id.clone(), answer);
    }
    let input = count_tokens(&serde_json::to_string(&request).unwrap_or_default());
    Ok(Reply::Json(json!({
        "model": if model.ends_with("-latest") { "jev-1.13.0" } else { model },
        "answers": answers,
        "usage": { "input_tokens": input, "output_tokens": 10 },
    })))
}

fn choice(q: &Value, text: &str) -> Value {
    let options: Vec<&String> =
        q.get("criteria").and_then(Value::as_object).map(|o| o.keys().collect()).unwrap_or_default();
    let found = options.iter().find(|o| **o != "unknown" && text.contains(&o.replace('_', " ")));
    let chosen = found.or_else(|| options.iter().find(|o| **o == "unknown")).or(options.first()).map(|o| o.as_str());
    let p = if found.is_some() { 0.9 } else { 0.4 };
    let rest = if options.len() > 1 { (1.0 - p) / (options.len() - 1) as f64 } else { 0.0 };
    let probabilities: Map<String, Value> =
        options.iter().map(|o| ((*o).clone(), json!(if Some(o.as_str()) == chosen { p } else { rest }))).collect();
    json!({ "type": "choice", "choice": chosen, "confidence": p, "probabilities": probabilities })
}
