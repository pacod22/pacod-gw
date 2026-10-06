//! Turns a stored routing scheme into the form the request path evaluates.
//!
//! The same function validates a scheme in the management API and compiles it into the
//! snapshot, so what the API accepts is exactly what runs.

use std::collections::HashSet;
use std::sync::Arc;

use crate::model::{
    Affinity, Complexity, ComplexityMatch, FailoverPolicy, RouteAction, RoutingScheme, StrMatch, is_slug,
};

use super::RouteInput;
#[cfg(feature = "routing-cel")]
use super::cel::CelRule;

pub const MAX_RULES: usize = 64;
/// Values of one `in` or `prefix` list.
const MAX_VALUES: usize = 64;
/// Attribution values are clipped to this length, so a longer one could never match.
const MAX_VALUE_LEN: usize = 200;
/// Tag conditions of one rule; a workspace declares at most 8 tag keys.
const MAX_TAGS: usize = 8;
/// Tiers one action names; a pool has at most 16 models.
const MAX_TIERS: usize = 16;
/// Up to this many exact values are compared one by one; more go into a hash set.
const SMALL_SET: usize = 8;

/// Why a scheme cannot be used. `rule` names the rule at fault, when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompileError {
    pub code: &'static str,
    pub rule: Option<String>,
    pub message: String,
}

impl std::fmt::Display for CompileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.rule {
            Some(r) => write!(f, "rule \"{r}\": {}", self.message),
            None => f.write_str(&self.message),
        }
    }
}

fn err(code: &'static str, rule: Option<&str>, message: impl Into<String>) -> CompileError {
    CompileError { code, rule: rule.map(str::to_owned), message: message.into() }
}

/// A compiled condition on one text value.
#[derive(Debug)]
pub(crate) enum StrPred {
    Any,
    Absent,
    Present,
    OneOf { exact: Exact, prefix: Box<[Box<str>]> },
}

#[derive(Debug)]
pub(crate) enum Exact {
    Small(Box<[Box<str>]>),
    Set(HashSet<Box<str>>),
}

impl StrPred {
    #[inline]
    pub(crate) fn matches(&self, v: Option<&str>) -> bool {
        match self {
            Self::Any => true,
            Self::Absent => v.is_none(),
            Self::Present => v.is_some(),
            Self::OneOf { exact, prefix } => {
                let Some(v) = v else { return false };
                let hit = match exact {
                    Exact::Small(vals) => vals.iter().any(|x| &**x == v),
                    Exact::Set(set) => set.contains(v),
                };
                hit || prefix.iter().any(|p| v.starts_with(&**p))
            }
        }
    }

    fn compile(m: &StrMatch, what: &str, rule: &str) -> Result<Self, CompileError> {
        let check = |vals: &[String], list: &str| -> Result<(), CompileError> {
            if vals.len() > MAX_VALUES {
                return Err(err(
                    "too_many_values",
                    Some(rule),
                    format!("{what}.{list} holds at most {MAX_VALUES} values"),
                ));
            }
            if let Some(v) = vals.iter().find(|v| v.is_empty() || v.chars().count() > MAX_VALUE_LEN) {
                return Err(err(
                    "invalid_value",
                    Some(rule),
                    format!("{what}.{list}: \"{v}\" must be 1 to {MAX_VALUE_LEN} characters"),
                ));
            }
            Ok(())
        };
        check(&m.one_of, "in")?;
        check(&m.prefix, "prefix")?;
        let listed = !m.one_of.is_empty() || !m.prefix.is_empty();
        Ok(match (m.present, listed) {
            (Some(false), true) => {
                return Err(err(
                    "contradiction",
                    Some(rule),
                    format!("{what}: present is false, so it cannot also list values"),
                ));
            }
            (Some(false), false) => Self::Absent,
            (Some(true), false) => Self::Present,
            (None, false) => Self::Any,
            (_, true) => {
                let exact = if m.one_of.len() <= SMALL_SET {
                    Exact::Small(m.one_of.iter().map(|s| s.as_str().into()).collect())
                } else {
                    Exact::Set(m.one_of.iter().map(|s| s.as_str().into()).collect())
                };
                Self::OneOf { exact, prefix: m.prefix.iter().map(|s| s.as_str().into()).collect() }
            }
        })
    }
}

/// Bit of a complexity in a rule's mask.
#[inline]
pub(crate) fn complexity_bit(c: Option<Complexity>) -> u8 {
    match c {
        Some(Complexity::Low) => 1,
        Some(Complexity::Medium) => 2,
        Some(Complexity::High) => 4,
        None => 8,
    }
}

/// A compiled rule. The action is bound to each template's pool separately.
#[derive(Debug)]
pub struct CompiledRule {
    pub name: Arc<str>,
    /// Bits of the complexities that match (see [`complexity_bit`]); 0 matches any.
    complexity: u8,
    agent_id: StrPred,
    task: StrPred,
    user_id: StrPred,
    template: StrPred,
    tags: Box<[(Box<str>, StrPred)]>,
    /// `[min, max)` of the estimated input tokens.
    input_tokens: (u32, u32),
    #[cfg(feature = "routing-cel")]
    cel: Option<Arc<CelRule>>,
    pub action: RouteAction,
    /// The rule's failover policy, or the scheme's.
    pub failover: FailoverPolicy,
}

impl CompiledRule {
    /// Whether the rule applies to a call. The cheap typed conditions come first; the CEL
    /// condition, if any, only when they all hold.
    #[inline]
    pub fn matches(&self, input: &RouteInput<'_>) -> bool {
        (self.complexity == 0 || self.complexity & complexity_bit(input.complexity) != 0)
            && (self.input_tokens.0..self.input_tokens.1).contains(&input.input_tokens)
            && self.agent_id.matches(input.agent_id)
            && self.task.matches(input.task)
            && self.user_id.matches(input.user_id)
            && self.template.matches(Some(input.template))
            && self.tags.iter().all(|(k, p)| p.matches(input.tags.get(&**k).map(String::as_str)))
            && self.cel_matches(input)
    }

    #[cfg(feature = "routing-cel")]
    #[inline]
    fn cel_matches(&self, input: &RouteInput<'_>) -> bool {
        self.cel.as_ref().is_none_or(|c| c.matches(input))
    }

    #[cfg(not(feature = "routing-cel"))]
    #[inline]
    fn cel_matches(&self, _input: &RouteInput<'_>) -> bool {
        true
    }

    pub fn has_cel(&self) -> bool {
        #[cfg(feature = "routing-cel")]
        return self.cel.is_some();
        #[cfg(not(feature = "routing-cel"))]
        false
    }
}

/// A scheme ready to be bound to templates.
#[derive(Debug)]
pub struct CompiledScheme {
    pub id: String,
    pub name: Arc<str>,
    /// The stored object's version, reported with each call.
    pub version: i32,
    pub enabled: bool,
    pub rules: Box<[CompiledRule]>,
    pub default: RouteAction,
    pub default_failover: FailoverPolicy,
}

fn check_failover(f: &FailoverPolicy, rule: Option<&str>) -> Result<(), CompileError> {
    if !(1..=8).contains(&f.max_targets) {
        return Err(err("invalid_failover", rule, "failover.max_targets must be between 1 and 8"));
    }
    if f.attempts_per_target.is_some_and(|a| !(1..=5).contains(&a)) {
        return Err(err("invalid_failover", rule, "failover.attempts_per_target must be between 1 and 5"));
    }
    if f.per_target_timeout_ms.is_some_and(|t| !(100..=600_000).contains(&t)) {
        return Err(err("invalid_failover", rule, "failover.per_target_timeout_ms must be between 100 and 600000"));
    }
    if f.deadline_ms.is_some_and(|t| !(100..=3_600_000).contains(&t)) {
        return Err(err("invalid_failover", rule, "failover.deadline_ms must be between 100 and 3600000"));
    }
    Ok(())
}

fn check_action(a: &RouteAction, rule: Option<&str>) -> Result<(), CompileError> {
    if a.tiers.len() > MAX_TIERS {
        return Err(err("invalid_tiers", rule, format!("an action names at most {MAX_TIERS} tiers")));
    }
    let mut seen = HashSet::new();
    for t in &a.tiers {
        if !is_slug(t) {
            return Err(err("invalid_tiers", rule, format!("the tier \"{t}\" must be a slug")));
        }
        if !seen.insert(t) {
            return Err(err("invalid_tiers", rule, format!("the tier \"{t}\" is named twice")));
        }
    }
    if a.affinity == Affinity::Session && a.algorithm == crate::model::Algorithm::Cheapest {
        return Err(err(
            "invalid_action",
            rule,
            "session affinity spreads sessions by weight; it cannot be combined with cheapest",
        ));
    }
    if let Some(f) = &a.failover {
        check_failover(f, rule)?;
    }
    Ok(())
}

/// Validates and compiles a scheme. `id` and `version` are the stored object's.
pub fn compile(id: &str, version: i32, s: &RoutingScheme) -> Result<CompiledScheme, CompileError> {
    if !is_slug(&s.name) {
        return Err(err("invalid_name", None, "name must be lower-case letters, digits and hyphens"));
    }
    if s.rules.len() > MAX_RULES {
        return Err(err("too_many_rules", None, format!("a scheme holds at most {MAX_RULES} rules")));
    }
    check_failover(&s.failover, None)?;
    check_action(&s.default, None)?;

    let mut names = HashSet::new();
    let mut rules = Vec::with_capacity(s.rules.len());
    for r in &s.rules {
        let name = r.name.as_str();
        if !is_slug(name) {
            return Err(err(
                "invalid_name",
                Some(name),
                "a rule's name must be lower-case letters, digits and hyphens",
            ));
        }
        if !names.insert(name) {
            return Err(err("duplicate_rule", Some(name), "rule names must be unique in the scheme"));
        }
        check_action(&r.then, Some(name))?;
        if r.when.tags.len() > MAX_TAGS {
            return Err(err("too_many_tags", Some(name), format!("a rule tests at most {MAX_TAGS} tags")));
        }
        let mut complexity = 0u8;
        for c in &r.when.complexity {
            complexity |= match c {
                ComplexityMatch::Low => 1,
                ComplexityMatch::Medium => 2,
                ComplexityMatch::High => 4,
                ComplexityMatch::Unset => 8,
            };
        }
        let t = r.when.input_tokens;
        let input_tokens = (t.at_least.unwrap_or(0), t.below.unwrap_or(u32::MAX));
        if input_tokens.0 >= input_tokens.1 {
            return Err(err("invalid_value", Some(name), "input_tokens.at_least must be below input_tokens.below"));
        }
        let mut tags = Vec::with_capacity(r.when.tags.len());
        for (k, m) in &r.when.tags {
            if k.is_empty() {
                return Err(err("invalid_value", Some(name), "a tag key must not be empty"));
            }
            tags.push((k.as_str().into(), StrPred::compile(m, &format!("tags.{k}"), name)?));
        }
        #[cfg(feature = "routing-cel")]
        let cel = match &r.cel {
            Some(src) => {
                Some(Arc::new(CelRule::compile(src, &s.name, name).map_err(|m| err("cel_invalid", Some(name), m))?))
            }
            None => None,
        };
        #[cfg(not(feature = "routing-cel"))]
        if r.cel.is_some() {
            return Err(err("cel_unavailable", Some(name), "this gateway was built without CEL conditions"));
        }
        rules.push(CompiledRule {
            name: name.into(),
            complexity,
            agent_id: StrPred::compile(&r.when.agent_id, "agent_id", name)?,
            task: StrPred::compile(&r.when.task, "task", name)?,
            user_id: StrPred::compile(&r.when.user_id, "user_id", name)?,
            template: StrPred::compile(&r.when.template, "template", name)?,
            tags: tags.into(),
            input_tokens,
            #[cfg(feature = "routing-cel")]
            cel,
            failover: r.then.failover.unwrap_or(s.failover),
            action: r.then.clone(),
        });
    }
    Ok(CompiledScheme {
        id: id.to_owned(),
        name: s.name.as_str().into(),
        version,
        enabled: s.status == crate::model::Status::Active,
        rules: rules.into(),
        default_failover: s.default.failover.unwrap_or(s.failover),
        default: s.default.clone(),
    })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use serde_json::json;

    fn scheme(v: serde_json::Value) -> RoutingScheme {
        serde_json::from_value(v).unwrap()
    }

    fn input<'a>(tags: &'a BTreeMap<String, String>) -> RouteInput<'a> {
        RouteInput { template: "bot", tags, ..RouteInput::empty() }
    }

    #[test]
    fn string_conditions() {
        let rule = |when: serde_json::Value| {
            let s = scheme(json!({ "name": "s", "rules": [{ "name": "r", "when": when, "then": {} }] }));
            compile("rsc_1", 1, &s).unwrap().rules.into_vec().remove(0)
        };
        let tags = BTreeMap::new();
        let with = |agent: Option<&'static str>| RouteInput { agent_id: agent, ..input(&tags) };

        let r = rule(json!({ "agent_id": { "in": ["a", "b"], "prefix": ["triage-"] } }));
        assert!(r.matches(&with(Some("a"))) && r.matches(&with(Some("triage-7"))));
        assert!(!r.matches(&with(Some("c"))) && !r.matches(&with(None)));

        let many: Vec<String> = (0..20).map(|i| format!("agent-{i}")).collect();
        let r = rule(json!({ "agent_id": { "in": many } }));
        assert!(r.matches(&with(Some("agent-13"))) && !r.matches(&with(Some("agent-20"))));

        let r = rule(json!({ "agent_id": { "present": false } }));
        assert!(r.matches(&with(None)) && !r.matches(&with(Some("a"))));
        let r = rule(json!({ "agent_id": { "present": true } }));
        assert!(!r.matches(&with(None)) && r.matches(&with(Some("a"))));
        let r = rule(json!({}));
        assert!(r.matches(&with(None)) && r.matches(&with(Some("a"))));
    }

    #[test]
    fn complexity_tags_and_template() {
        let s = scheme(json!({ "name": "s", "rules": [{ "name": "r",
            "when": { "complexity": ["low", "unset"], "tags": { "team": { "in": ["search"] } }, "template": { "prefix": ["b"] } },
            "then": {} }] }));
        let c = compile("rsc_1", 1, &s).unwrap();
        let r = &c.rules[0];
        let mut tags = BTreeMap::new();
        assert!(!r.matches(&input(&tags)), "the tag is missing");
        tags.insert("team".to_owned(), "search".to_owned());
        assert!(r.matches(&input(&tags)), "no complexity counts as unset");
        assert!(r.matches(&RouteInput { complexity: Some(Complexity::Low), ..input(&tags) }));
        assert!(!r.matches(&RouteInput { complexity: Some(Complexity::High), ..input(&tags) }));
        assert!(!r.matches(&RouteInput { template: "other", ..input(&tags) }));

        let s = scheme(json!({ "name": "s", "rules": [{ "name": "long",
            "when": { "input_tokens": { "at_least": 32000 } }, "then": {} }] }));
        let c = compile("rsc_1", 1, &s).unwrap();
        assert!(c.rules[0].matches(&RouteInput { input_tokens: 32_000, ..input(&tags) }));
        assert!(!c.rules[0].matches(&RouteInput { input_tokens: 31_999, ..input(&tags) }));
        let bad = scheme(json!({ "name": "s", "rules": [{ "name": "x",
            "when": { "input_tokens": { "at_least": 10, "below": 10 } }, "then": {} }] }));
        assert_eq!(compile("rsc_1", 1, &bad).unwrap_err().code, "invalid_value");
    }

    #[test]
    fn invalid_schemes_are_refused_with_the_rule() {
        let bad = |v: serde_json::Value| compile("rsc_1", 1, &scheme(v)).unwrap_err();
        let e = bad(json!({ "name": "s", "rules": [
            { "name": "a", "then": {} }, { "name": "a", "then": {} } ] }));
        assert_eq!((e.code, e.rule.as_deref()), ("duplicate_rule", Some("a")));
        let e = bad(json!({ "name": "s", "rules": [
            { "name": "x", "when": { "task": { "present": false, "in": ["t"] } }, "then": {} } ] }));
        assert_eq!(e.code, "contradiction");
        let e = bad(json!({ "name": "s", "rules": [
            { "name": "x", "then": { "tiers": ["Small"] } } ] }));
        assert_eq!(e.code, "invalid_tiers");
        let e = bad(json!({ "name": "s", "failover": { "max_targets": 0 } }));
        assert_eq!((e.code, e.rule), ("invalid_failover", None));
        let e = bad(json!({ "name": "s", "rules": [
            { "name": "x", "then": { "failover": { "attempts_per_target": 9 } } } ] }));
        assert_eq!((e.code, e.rule.as_deref()), ("invalid_failover", Some("x")));
        let e = bad(json!({ "name": "Bad" }));
        assert_eq!(e.code, "invalid_name");
    }

    #[cfg(feature = "routing-cel")]
    #[test]
    fn a_cel_condition_comes_after_the_typed_ones() {
        let s = scheme(json!({ "name": "s", "rules": [{ "name": "long",
            "when": { "task": { "present": true } }, "cel": "input_tokens > 1000 && !stream", "then": {} }] }));
        let c = compile("rsc_1", 1, &s).unwrap();
        let tags = BTreeMap::new();
        let r = &c.rules[0];
        assert!(r.has_cel());
        let call = |task, input_tokens| RouteInput { task, input_tokens, ..input(&tags) };
        assert!(r.matches(&call(Some("summarise"), 5_000)));
        assert!(!r.matches(&call(Some("summarise"), 500)));
        assert!(!r.matches(&call(None, 5_000)), "the typed condition fails first");
        let bad = scheme(json!({ "name": "s", "rules": [{ "name": "x", "cel": "body.size > 1", "then": {} }] }));
        let e = compile("rsc_1", 1, &bad).unwrap_err();
        assert_eq!((e.code, e.rule.as_deref()), ("cel_invalid", Some("x")));
    }

    #[test]
    fn a_rule_without_its_own_failover_takes_the_schemes() {
        let s = scheme(json!({ "name": "s", "failover": { "max_targets": 5 }, "rules": [
            { "name": "a", "then": {} },
            { "name": "b", "then": { "failover": { "max_targets": 1 } } } ] }));
        let c = compile("rsc_1", 4, &s).unwrap();
        assert_eq!((c.rules[0].failover.max_targets, c.rules[1].failover.max_targets), (5, 1));
        assert_eq!((c.default_failover.max_targets, c.version), (5, 4));
    }
}
