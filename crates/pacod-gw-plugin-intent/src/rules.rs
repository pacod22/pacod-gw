//! An engine's answer, and the rules that act on it (`docs/plugin-concept.md`, section 5.8).

use std::collections::BTreeMap;

use pacod_gw_plugin::{Action, Trial, TrialAction};
use serde_json::Value;

use crate::catalogue::{Catalogue, RuleAction, When};

/// The error type of a blocked call.
pub const BLOCKED: &str = "intent_blocked";

/// What the caller is told when a rule without a message blocks a call.
pub const DEFAULT_MESSAGE: &str = "This request is not allowed in this workspace.";

/// What an engine found, whichever engine it was.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// A label of the catalogue, or `unknown`.
    pub label: String,
    pub confidence: f32,
    /// The probability of yes, by flag id.
    pub flags: BTreeMap<String, f32>,
    /// From 0 (the first level) to 1 (the last), by score id.
    pub scores: BTreeMap<String, f32>,
    /// The option chosen, by choice id.
    pub choices: BTreeMap<String, String>,
    /// `llm`, `jev`, `cascade:jev` or `cascade:llm`.
    pub engine: String,
    /// For the audit record: the probabilities, the rationale, the versions that answered.
    pub detail: Value,
}

/// What the rules decided.
#[derive(Debug, Clone, PartialEq)]
pub struct Decision {
    pub action: Action,
    pub flag: bool,
    pub alerts: Vec<String>,
    pub rule: Option<String>,
    pub trial: Vec<Trial>,
}

fn applies(when: &When, a: &Answer) -> bool {
    (when.intents.is_empty() || when.intents.contains(&a.label))
        && when.confidence.is_none_or(|r| r.contains(a.confidence))
        && when.flags.iter().all(|(id, r)| a.flags.get(id).is_some_and(|v| r.contains(*v)))
        && when.scores.iter().all(|(id, r)| a.scores.get(id).is_some_and(|v| r.contains(*v)))
        && when.choices.iter().all(|(id, options)| a.choices.get(id).is_some_and(|c| options.contains(c)))
}

/// Applies the catalogue's rules in order. The first `allow` or `block` that matches decides;
/// every matching `flag` marks the call. Rules in trial are recorded, never acted on. A call
/// no rule decides is allowed.
pub fn decide(c: &Catalogue, a: &Answer) -> Decision {
    let mut d = Decision { action: Action::Allow, flag: false, alerts: Vec::new(), rule: None, trial: Vec::new() };
    for r in c.rules.iter().filter(|r| applies(&r.when, a)) {
        if r.shadow {
            match r.then.action {
                RuleAction::Block => d.trial.push(Trial { rule: r.name.clone(), action: TrialAction::Block }),
                RuleAction::Flag => d.trial.push(Trial { rule: r.name.clone(), action: TrialAction::Flag }),
                RuleAction::Allow => {}
            }
            continue;
        }
        match r.then.action {
            RuleAction::Flag => {
                d.flag = true;
                if r.then.alert {
                    d.alerts.push(r.name.clone());
                }
            }
            RuleAction::Allow => {
                d.rule = Some(r.name.clone());
                break;
            }
            RuleAction::Block => {
                d.rule = Some(r.name.clone());
                d.action = Action::Block {
                    code: BLOCKED,
                    message: r
                        .then
                        .message
                        .clone()
                        .filter(|m| !m.trim().is_empty())
                        .unwrap_or_else(|| DEFAULT_MESSAGE.into()),
                };
                break;
            }
        }
    }
    d
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn catalogue() -> Catalogue {
        serde_json::from_value(json!({
            "name": "c",
            "intents": [{ "name": "billing", "description": "x" }, { "name": "legal_advice", "description": "x" }],
            "questions": [{ "id": "jailbreak", "kind": "flag", "instructions": "x" }],
            "rules": [
                { "name": "watch", "when": { "flags": { "jailbreak": { "min": 0.8 } } }, "then": { "action": "flag", "alert": true } },
                { "name": "trial", "shadow": true, "when": { "intents": ["unknown"] }, "then": { "action": "block" } },
                { "name": "legal", "when": { "intents": ["legal_advice"], "confidence": { "min": 0.5 } },
                  "then": { "action": "block", "message": "No legal advice here." } },
                { "name": "fine", "when": { "intents": ["legal_advice"] }, "then": { "action": "allow" } },
                { "name": "never", "when": {}, "then": { "action": "block" } }
            ]
        }))
        .unwrap()
    }

    fn answer(label: &str, confidence: f32, jailbreak: f32) -> Answer {
        Answer {
            label: label.into(),
            confidence,
            flags: [("jailbreak".to_owned(), jailbreak)].into(),
            scores: BTreeMap::new(),
            choices: BTreeMap::new(),
            engine: "llm".into(),
            detail: Value::Null,
        }
    }

    #[test]
    fn rules_apply_in_order() {
        let c = catalogue();
        let d = decide(&c, &answer("legal_advice", 0.9, 0.9));
        assert_eq!(d.action, Action::Block { code: BLOCKED, message: "No legal advice here.".into() });
        assert_eq!(
            (d.flag, d.alerts.clone(), d.rule.clone()),
            (true, vec!["watch".to_owned()], Some("legal".to_owned()))
        );

        let d = decide(&c, &answer("legal_advice", 0.3, 0.0));
        assert_eq!(
            (d.action.clone(), d.rule.clone()),
            (Action::Allow, Some("fine".to_owned())),
            "an allow ends the rules"
        );

        let d = decide(&c, &answer("unknown", 0.9, 0.0));
        assert_eq!(d.trial, vec![Trial { rule: "trial".into(), action: TrialAction::Block }]);
        assert_eq!(d.action, Action::Block { code: BLOCKED, message: DEFAULT_MESSAGE.into() }, "the catch-all");
    }
}
