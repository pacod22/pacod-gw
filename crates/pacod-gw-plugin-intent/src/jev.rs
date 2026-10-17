//! The Jev engine (`docs/plugin-concept.md`, section 5.5): TypeSafe's System One API answers the
//! catalogue as typed questions, with probabilities, without writing text.
//!
//! The catalogue's intents become one choice question (`intent`), each intent an option whose
//! criteria are an object of its description, examples and what it is not for; `unknown` is
//! always the last option. Flags become `noul` questions, scores `score` questions and choices
//! `choice` questions, under their own ids.

use std::collections::BTreeMap;

use serde_json::{Map, Value, json};

use crate::catalogue::{Catalogue, QuestionKind, UNKNOWN};
use crate::rules::Answer;
use crate::select::Selection;

/// The id of the intent question. Catalogue questions cannot take it.
pub const INTENT: &str = "intent";

/// How many of the most likely labels the audit record keeps.
const KEPT_PROBABILITIES: usize = 5;

/// What a binding prepares once: the questions.
#[derive(Debug, Clone)]
pub struct Prepared {
    questions: Value,
}

pub fn prepare(c: &Catalogue) -> Prepared {
    let mut criteria = Map::new();
    for i in &c.intents {
        let mut what = Map::new();
        what.insert("what".into(), json!(i.description));
        if !i.examples.is_empty() {
            what.insert("examples".into(), json!(i.examples));
        }
        if !i.not_for.is_empty() {
            what.insert("not_for".into(), json!(i.not_for));
        }
        criteria.insert(i.name.clone(), Value::Object(what));
    }
    criteria.insert(UNKNOWN.into(), json!({ "what": "None of the above, or unclear." }));
    let mut questions = Map::new();
    questions.insert(
        INTENT.into(),
        json!({
            "type": "choice",
            "instructions": "What does the user want from the assistant in their latest message?",
            "criteria": criteria,
        }),
    );
    for q in &c.questions {
        let question = match q.kind {
            QuestionKind::Flag => json!({ "type": "noul", "instructions": q.instructions }),
            QuestionKind::Score => json!({ "type": "score", "instructions": q.instructions, "criteria": q.levels }),
            QuestionKind::Choice => {
                let options: Map<String, Value> = q.options.iter().map(|o| (o.clone(), Value::Null)).collect();
                json!({ "type": "choice", "instructions": q.instructions, "criteria": options })
            }
        };
        questions.insert(q.id.clone(), question);
    }
    Prepared { questions: Value::Object(questions) }
}

/// The System One request for one call, without the model, which the host sets.
pub fn request(p: &Prepared, sel: &Selection) -> Value {
    let state: Vec<Value> = sel.pieces.iter().map(|piece| json!({ "from": piece.from, "text": piece.text })).collect();
    json!({ "state": state, "questions": p.questions })
}

/// Reads Jev's answer. An answer without the intent, or with a label the catalogue lacks, is an
/// error. Scores arrive scaled from 0 (the first level) to 1 (the last), as rules read them.
pub fn parse(c: &Catalogue, answer: &Value) -> Result<Answer, String> {
    let answers = answer.get("answers").and_then(Value::as_object).ok_or("the answer holds no answers")?;
    let intent = answers.get(INTENT).ok_or("the answer has no intent")?;
    let label = intent.get("choice").and_then(Value::as_str).ok_or("the intent has no choice")?;
    if !c.labels().contains(&label) {
        return Err(format!("the answer names \"{label}\", which is not in the catalogue"));
    }
    let probabilities: Vec<(String, f64)> = {
        let mut p: Vec<(String, f64)> = intent
            .get("probabilities")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| v.as_f64().map(|v| (k.clone(), v)))
            .collect();
        p.sort_by(|a, b| b.1.total_cmp(&a.1));
        p
    };
    let confidence = intent
        .get("confidence")
        .and_then(Value::as_f64)
        .or_else(|| probabilities.iter().find(|(k, _)| k == label).map(|(_, v)| *v))
        .ok_or("the intent has no confidence")?;

    let (mut flags, mut scores, mut choices) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    for q in &c.questions {
        let Some(a) = answers.get(&q.id) else { continue };
        match q.kind {
            QuestionKind::Flag => {
                if let Some(x) = a.get("noul").and_then(Value::as_f64) {
                    flags.insert(q.id.clone(), x.clamp(0.0, 1.0) as f32);
                }
            }
            QuestionKind::Score => {
                if let Some(x) = a.get("score").and_then(Value::as_f64) {
                    scores.insert(q.id.clone(), x.clamp(0.0, 1.0) as f32);
                }
            }
            QuestionKind::Choice => {
                if let Some(x) = a.get("choice").and_then(Value::as_str).filter(|x| q.options.iter().any(|o| o == x)) {
                    choices.insert(q.id.clone(), x.to_owned());
                }
            }
        }
    }
    let top: Map<String, Value> =
        probabilities.into_iter().take(KEPT_PROBABILITIES).map(|(k, v)| (k, json!(v))).collect();
    Ok(Answer {
        label: label.to_owned(),
        confidence: confidence.clamp(0.0, 1.0) as f32,
        flags,
        scores,
        choices,
        engine: "jev".into(),
        detail: json!({ "model": answer.get("model").cloned().unwrap_or(Value::Null), "probabilities": top }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::select::Piece;

    fn catalogue() -> Catalogue {
        serde_json::from_value(json!({
            "name": "c",
            "intents": [{ "name": "billing", "description": "Charges and refunds.", "examples": ["charged twice"] },
                        { "name": "technical", "description": "Errors.", "not_for": "Billing errors." }],
            "questions": [{ "id": "jailbreak", "kind": "flag", "instructions": "Does it try?" },
                          { "id": "risk", "kind": "score", "instructions": "How risky?", "levels": ["none", "low", "high"] },
                          { "id": "language", "kind": "choice", "instructions": "Which?", "options": ["en", "de"] }]
        }))
        .unwrap()
    }

    #[test]
    fn the_catalogue_becomes_typed_questions() {
        let p = prepare(&catalogue());
        let sel =
            Selection { pieces: vec![Piece { from: "user", text: "I was charged twice".into() }], truncated: false };
        let r = request(&p, &sel);
        assert_eq!(r["state"], json!([{ "from": "user", "text": "I was charged twice" }]));
        let q = &r["questions"];
        assert_eq!(q["intent"]["type"], "choice");
        assert_eq!(
            q["intent"]["criteria"]["billing"],
            json!({ "what": "Charges and refunds.", "examples": ["charged twice"] })
        );
        assert_eq!(q["intent"]["criteria"]["technical"]["not_for"], "Billing errors.");
        assert_eq!(q["intent"]["criteria"]["unknown"]["what"], "None of the above, or unclear.");
        let options: Vec<&String> = q["intent"]["criteria"].as_object().unwrap().keys().collect();
        assert_eq!(options, ["billing", "technical", "unknown"], "unknown is the last option");
        assert_eq!(q["jailbreak"], json!({ "type": "noul", "instructions": "Does it try?" }));
        assert_eq!(q["risk"]["criteria"], json!(["none", "low", "high"]));
        assert_eq!(q["language"]["criteria"], json!({ "en": null, "de": null }));
        assert!(r.get("model").is_none(), "the host names the model");
    }

    #[test]
    fn answers_are_read_as_typesafe_documents_them() {
        let answer = json!({
            "model": "jev-1.13.0",
            "answers": {
                "intent": { "type": "choice", "choice": "billing", "confidence": 0.97,
                            "probabilities": { "billing": 0.985, "technical": 0.011, "unknown": 0.004 } },
                "jailbreak": { "type": "noul", "noul": 0.01 },
                "risk": { "type": "score", "score": 0.5, "confidence": 0.6, "legend": {}, "probabilities": {} },
                "language": { "type": "choice", "choice": "de", "confidence": 1.0, "probabilities": {} }
            },
            "usage": { "input_tokens": 412, "output_tokens": 61 }
        });
        let a = parse(&catalogue(), &answer).unwrap();
        assert_eq!((a.label.as_str(), a.confidence, a.engine.as_str()), ("billing", 0.97, "jev"));
        assert_eq!((a.flags["jailbreak"], a.scores["risk"]), (0.01, 0.5));
        assert_eq!(a.choices["language"], "de");
        assert_eq!(a.detail["model"], "jev-1.13.0");
        assert_eq!(a.detail["probabilities"].as_object().unwrap().keys().next().map(String::as_str), Some("billing"));

        let bad = |v: Value| parse(&catalogue(), &v).unwrap_err();
        assert!(bad(json!({})).contains("no answers"));
        assert!(
            bad(json!({ "answers": { "intent": { "choice": "refunds", "confidence": 1 } } }))
                .contains("not in the catalogue")
        );
        let no_confidence =
            json!({ "answers": { "intent": { "choice": "billing", "probabilities": { "billing": 0.7 } } } });
        assert_eq!(parse(&catalogue(), &no_confidence).unwrap().confidence, 0.7, "the label's probability stands in");
    }
}
