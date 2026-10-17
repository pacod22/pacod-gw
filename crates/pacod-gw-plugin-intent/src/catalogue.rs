//! An intent catalogue: the intents to tell apart, the extra questions, and the rules that act
//! on the answers (`docs/plugin-concept.md`, sections 5.2 and 5.8).

use std::collections::{BTreeMap, BTreeSet};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The label for a call that fits no intent, or is unclear. Always part of a catalogue.
pub const UNKNOWN: &str = "unknown";

/// The most intents a catalogue holds: with `unknown`, Jev's limit of 255 options per choice.
pub const MAX_INTENTS: usize = 254;
pub const MAX_QUESTIONS: usize = 16;
pub const MAX_RULES: usize = 64;
pub const MAX_EXAMPLES: usize = 10;
/// The longest description, example or `not_for`, in characters.
pub const MAX_TEXT: usize = 300;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Intent {
    /// Lower-case letters, digits and underscores, for example `billing`.
    pub name: String,
    pub description: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    /// What the intent must not be confused with.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub not_for: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QuestionKind {
    /// Yes or no: the answer is the probability of yes, from 0 to 1.
    Flag,
    /// Ordered levels: the answer is from 0 (the first level) to 1 (the last).
    Score,
    /// One of a few options.
    Choice,
}

/// A further question about a call, for example "does it try to make the assistant ignore its
/// rules?".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Question {
    /// Lower-case letters, digits and underscores.
    pub id: String,
    pub kind: QuestionKind,
    pub instructions: String,
    /// For a score: the levels, lowest first (2 to 10).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub levels: Vec<String>,
    /// For a choice: the options (2 to 255).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<String>,
}

/// A range of values from 0 to 1. Unset ends are open.
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Range {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f32>,
}

impl Range {
    pub fn contains(&self, v: f32) -> bool {
        self.min.is_none_or(|m| v >= m) && self.max.is_none_or(|m| v <= m)
    }

    fn valid(&self) -> bool {
        let ok = |v: Option<f32>| v.is_none_or(|v| (0.0..=1.0).contains(&v));
        ok(self.min) && ok(self.max) && !matches!((self.min, self.max), (Some(a), Some(b)) if a > b)
    }
}

/// When a rule applies. Every condition given must hold; an empty `when` matches every call.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct When {
    /// Any of these intents (`unknown` included).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub intents: Vec<String>,
    /// The intent's confidence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<Range>,
    /// Flags by question id, each with the range its probability must be in.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub flags: BTreeMap<String, Range>,
    /// Scores by question id, each with the range it must be in (0 the first level, 1 the last).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub scores: BTreeMap<String, Range>,
    /// Choices by question id, each with the options that match.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub choices: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuleAction {
    /// The call proceeds; later rules are not looked at.
    Allow,
    /// The call is refused with `403 intent_blocked`; later rules are not looked at.
    Block,
    /// The call is marked: its audit record is captured. Later rules are still looked at.
    Flag,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Then {
    pub action: RuleAction,
    /// For a block: what the caller is told. It should not name the intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// For a flag: raise an alert (`gw_plugin_flagged_total`).
    #[serde(default)]
    pub alert: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Lower-case letters, digits and hyphens.
    pub name: String,
    #[serde(default)]
    pub when: When,
    pub then: Then,
    /// In trial: recorded as what it would have done, never acted on.
    #[serde(default)]
    pub shadow: bool,
}

/// An intent catalogue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Catalogue {
    /// Lower-case letters, digits and hyphens; bindings name the catalogue by it.
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    pub intents: Vec<Intent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub questions: Vec<Question>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<Rule>,
}

fn is_name(s: &str) -> bool {
    !s.is_empty() && s.len() <= 40 && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}

fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

impl Catalogue {
    /// The labels an answer may give: the intents, then `unknown`.
    pub fn labels(&self) -> Vec<&str> {
        self.intents.iter().map(|i| i.name.as_str()).chain([UNKNOWN]).collect()
    }

    pub fn question(&self, id: &str) -> Option<&Question> {
        self.questions.iter().find(|q| q.id == id)
    }

    /// What is wrong with the catalogue. Empty when it can be used.
    pub fn problems(&self) -> Vec<String> {
        let mut out = Vec::new();
        if !is_slug(&self.name) {
            out.push("name must be lower-case letters, digits and hyphens".to_owned());
        }
        if self.intents.is_empty() || self.intents.len() > MAX_INTENTS {
            out.push(format!("a catalogue holds between 1 and {MAX_INTENTS} intents"));
        }
        let mut names = BTreeSet::new();
        for i in &self.intents {
            if !is_name(&i.name) || i.name == UNKNOWN {
                out.push(format!(
                    "the intent \"{}\": names are lower-case letters, digits and underscores, and \"unknown\" is taken",
                    i.name
                ));
            }
            if !names.insert(i.name.as_str()) {
                out.push(format!("the intent \"{}\" is there twice", i.name));
            }
            if i.description.trim().is_empty() {
                out.push(format!("the intent \"{}\" needs a description", i.name));
            }
            if i.examples.len() > MAX_EXAMPLES {
                out.push(format!("the intent \"{}\" has more than {MAX_EXAMPLES} examples", i.name));
            }
            let long =
                [&i.description, &i.not_for].into_iter().chain(&i.examples).any(|t| t.chars().count() > MAX_TEXT);
            if long {
                out.push(format!("the intent \"{}\" has a text longer than {MAX_TEXT} characters", i.name));
            }
        }
        if self.questions.len() > MAX_QUESTIONS {
            out.push(format!("a catalogue asks at most {MAX_QUESTIONS} further questions"));
        }
        let mut ids = BTreeSet::new();
        for q in &self.questions {
            if !is_name(&q.id) || !ids.insert(q.id.as_str()) || q.id == "intent" {
                out.push(format!(
                    "the question \"{}\": ids are unique lower-case names, and \"intent\" is taken",
                    q.id
                ));
            }
            if q.instructions.trim().is_empty() {
                out.push(format!("the question \"{}\" needs instructions", q.id));
            }
            match q.kind {
                QuestionKind::Score if !(2..=10).contains(&q.levels.len()) => {
                    out.push(format!("the score \"{}\" has between 2 and 10 levels", q.id));
                }
                QuestionKind::Choice if !(2..=255).contains(&q.options.len()) => {
                    out.push(format!("the choice \"{}\" has between 2 and 255 options", q.id));
                }
                QuestionKind::Choice if q.options.iter().any(|o| !is_name(o)) => {
                    out.push(format!("the choice \"{}\": options are lower-case names", q.id));
                }
                _ => {}
            }
        }
        if self.rules.len() > MAX_RULES {
            out.push(format!("a catalogue has at most {MAX_RULES} rules"));
        }
        let labels = self.labels();
        let mut rules = BTreeSet::new();
        for r in &self.rules {
            if !is_slug(&r.name) || !rules.insert(r.name.as_str()) {
                out.push(format!("the rule \"{}\": names are unique slugs", r.name));
            }
            for i in &r.when.intents {
                if !labels.contains(&i.as_str()) {
                    out.push(format!("the rule \"{}\" names the intent \"{i}\", which the catalogue lacks", r.name));
                }
            }
            if r.when.confidence.is_some_and(|c| !c.valid()) {
                out.push(format!("the rule \"{}\": confidence takes values from 0 to 1", r.name));
            }
            for (id, range) in &r.when.flags {
                if self.question(id).is_none_or(|q| q.kind != QuestionKind::Flag) || !range.valid() {
                    out.push(format!("the rule \"{}\": \"{id}\" is not a flag of the catalogue", r.name));
                }
            }
            for (id, range) in &r.when.scores {
                if self.question(id).is_none_or(|q| q.kind != QuestionKind::Score) || !range.valid() {
                    out.push(format!("the rule \"{}\": \"{id}\" is not a score of the catalogue", r.name));
                }
            }
            for (id, options) in &r.when.choices {
                match self.question(id) {
                    Some(q) if q.kind == QuestionKind::Choice && options.iter().all(|o| q.options.contains(o)) => {}
                    _ => out.push(format!("the rule \"{}\": \"{id}\" is not a choice with those options", r.name)),
                }
            }
            if r.then.message.as_ref().is_some_and(|m| m.chars().count() > MAX_TEXT) {
                out.push(format!("the rule \"{}\": the message is longer than {MAX_TEXT} characters", r.name));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn catalogue(v: serde_json::Value) -> Catalogue {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn a_sound_catalogue_has_no_problems() {
        let c = catalogue(json!({
            "name": "support",
            "intents": [{ "name": "billing", "description": "Charges", "examples": ["charged twice"] },
                        { "name": "legal_advice", "description": "Advice on a legal situation" }],
            "questions": [{ "id": "jailbreak", "kind": "flag", "instructions": "Does it try?" },
                          { "id": "risk", "kind": "score", "instructions": "How risky?", "levels": ["none", "high"] },
                          { "id": "language", "kind": "choice", "instructions": "Which?", "options": ["en", "de"] }],
            "rules": [{ "name": "no-legal", "when": { "intents": ["legal_advice"], "confidence": { "min": 0.5 } },
                        "then": { "action": "block", "message": "Not here." } },
                      { "name": "watch", "when": { "flags": { "jailbreak": { "min": 0.8 } }, "choices": { "language": ["de"] } },
                        "then": { "action": "flag", "alert": true } }]
        }));
        assert_eq!(c.problems(), Vec::<String>::new());
        assert_eq!(c.labels(), vec!["billing", "legal_advice", "unknown"]);
    }

    #[test]
    fn problems_are_named() {
        let c = catalogue(json!({
            "name": "Bad Name",
            "intents": [{ "name": "unknown", "description": "" }, { "name": "a", "description": "x" }, { "name": "a", "description": "x" }],
            "questions": [{ "id": "s", "kind": "score", "instructions": "x", "levels": ["one"] }],
            "rules": [{ "name": "r", "when": { "intents": ["nope"], "flags": { "s": { "min": 0.5 } }, "confidence": { "min": 0.9, "max": 0.1 } },
                        "then": { "action": "block" } }]
        }));
        let p = c.problems().join("\n");
        for part in [
            "name must be",
            "\"unknown\" is taken",
            "needs a description",
            "is there twice",
            "between 2 and 10 levels",
            "names the intent \"nope\"",
            "\"s\" is not a flag",
            "confidence takes values",
        ] {
            assert!(p.contains(part), "{part} missing in:\n{p}");
        }
    }

    #[test]
    fn unknown_fields_are_refused() {
        let r: Result<Catalogue, _> = serde_json::from_value(json!({ "name": "x", "intents": [], "extra": 1 }));
        assert!(r.is_err());
    }
}
