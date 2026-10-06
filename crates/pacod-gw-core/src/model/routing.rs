//! Routing schemes: how a template with several models chooses the one that serves a call.
//!
//! A template holds the pool of models, each with an optional tier, a weight and a priority
//! (see [`super::template::PoolEntry`]). A scheme is reusable behaviour defined by an
//! organisation: ordered rules that look at what the caller says about a call (complexity,
//! agent, task, tags) and choose tiers of the pool, an algorithm and a failover policy. A
//! scheme is assigned to an organisation, a workspace or a template; the most specific
//! assignment applies.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{Doc, Status};

/// How demanding the caller says a call is. A closed set, so that it can be a metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Complexity {
    Low,
    Medium,
    High,
}

impl Complexity {
    pub fn name(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

/// A complexity a rule matches. `unset` matches calls that state none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ComplexityMatch {
    Low,
    Medium,
    High,
    Unset,
}

/// A condition on one text value of a call. Empty matches any value, including none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct StrMatch {
    /// The value is one of these, exactly.
    #[serde(rename = "in", skip_serializing_if = "Vec::is_empty")]
    pub one_of: Vec<String>,
    /// The value starts with one of these. Combined with `in`, either may match.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub prefix: Vec<String>,
    /// `true`: the call must give a value; `false`: it must give none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub present: Option<bool>,
}

impl StrMatch {
    pub fn is_any(&self) -> bool {
        self.one_of.is_empty() && self.prefix.is_empty() && self.present.is_none()
    }
}

/// When a rule applies. Every condition given must hold.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RouteMatch {
    /// Any of these complexities. Empty matches any.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub complexity: Vec<ComplexityMatch>,
    /// The calling agent (`x-gw-agent-id`, `gateway.agent_id`).
    #[serde(skip_serializing_if = "StrMatch::is_any")]
    pub agent_id: StrMatch,
    /// The task (`x-gw-task`, `gateway.task`).
    #[serde(skip_serializing_if = "StrMatch::is_any")]
    pub task: StrMatch,
    /// The application's user (`x-gw-user-id`, `gateway.user_id`).
    #[serde(skip_serializing_if = "StrMatch::is_any")]
    pub user_id: StrMatch,
    /// The template's name, for a scheme shared by several templates.
    #[serde(skip_serializing_if = "StrMatch::is_any")]
    pub template: StrMatch,
    /// Tags of the call, by key.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub tags: BTreeMap<String, StrMatch>,
}

/// How the model that serves first is picked among the chosen candidates.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Algorithm {
    /// Main and backups: the lowest priority serves, the next ones take over on failure.
    #[default]
    Priority,
    /// A random pick in proportion to the weights.
    Weighted,
    /// Each in turn.
    RoundRobin,
    /// In turn, in proportion to the weights, evenly interleaved.
    WeightedRoundRobin,
    /// The fewer calls in flight (relative to the weight), of two weighted random picks.
    LeastInFlight,
    /// The lowest time to the first byte, measured by this instance; occasionally another one,
    /// to keep the measurements fresh.
    LowestLatency,
    /// The lowest price per token.
    Cheapest,
}

impl Algorithm {
    pub fn name(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::Weighted => "weighted",
            Self::RoundRobin => "round_robin",
            Self::WeightedRoundRobin => "weighted_round_robin",
            Self::LeastInFlight => "least_in_flight",
            Self::LowestLatency => "lowest_latency",
            Self::Cheapest => "cheapest",
        }
    }
}

/// Whether calls of one session stay on one model.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Affinity {
    #[default]
    None,
    /// Calls with the same session id (`x-gw-session`) go to the same model while it is
    /// healthy, which keeps the provider's prompt cache warm.
    Session,
}

/// What follows the chosen tiers when all of them fail.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Overflow {
    /// Nothing: the call fails.
    #[default]
    None,
    /// The rest of the pool, by priority.
    RestOfPool,
}

/// Which failures move a call on to the next model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FailoverOn {
    /// `429` from the provider.
    pub rate_limited: bool,
    /// `5xx` (and Anthropic's `529`) from the provider.
    pub server_error: bool,
    /// The provider could not be reached.
    pub connect_error: bool,
    /// A limit of the gateway that counts only this model refused the call.
    pub limit_refused: bool,
    /// The provider did not answer in time. Off by default: the provider may be generating,
    /// and billing, the answer already.
    pub timeout: bool,
}

impl Default for FailoverOn {
    fn default() -> Self {
        Self { rate_limited: true, server_error: true, connect_error: true, limit_refused: true, timeout: false }
    }
}

/// When and how far a call moves on to another model. Failover happens only before the first
/// byte of the answer has reached the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct FailoverPolicy {
    pub on: FailoverOn,
    /// How many different models one call may try, including the first. 1 to 8.
    pub max_targets: u8,
    /// Attempts on each model. Unset: the template's `retry_max_attempts` for a pool of one
    /// model, otherwise 1, since the next model is usually the better retry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attempts_per_target: Option<u8>,
    /// Caps the time each model gets, to leave time for the next.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub per_target_timeout_ms: Option<u32>,
    /// No further model is tried once this much time has passed since the call arrived.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u32>,
}

impl Default for FailoverPolicy {
    fn default() -> Self {
        Self {
            on: FailoverOn::default(),
            max_targets: 3,
            attempts_per_target: None,
            per_target_timeout_ms: None,
            deadline_ms: None,
        }
    }
}

/// What a rule does: which models are candidates, and how the first is picked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct RouteAction {
    /// Tiers of the template's pool, in order of preference. Empty: the whole pool.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<String>,
    pub algorithm: Algorithm,
    pub affinity: Affinity,
    pub overflow: Overflow,
    /// Overrides the scheme's failover policy for this rule.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failover: Option<FailoverPolicy>,
}

/// One rule of a scheme.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RouteRule {
    /// Slug, unique in the scheme. Reported in `x-gw-route`.
    pub name: String,
    #[serde(default)]
    pub when: RouteMatch,
    /// A CEL condition on top of `when`, for what the typed conditions cannot say. Variables:
    /// `complexity`, `agent_id`, `task`, `user_id`, `end_user`, `session_id`, `template`,
    /// `interface`, `stream`, `input_tokens` and the `tags` map; text values are empty when
    /// not given. An expression that fails to evaluate does not match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cel: Option<String>,
    pub then: RouteAction,
}

/// A routing scheme of an organisation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RoutingScheme {
    /// Slug, unique in the organisation. Organisations, workspaces and templates name it.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Tried in order; the first that matches decides.
    #[serde(default)]
    pub rules: Vec<RouteRule>,
    /// What applies when no rule matches.
    #[serde(default)]
    pub default: RouteAction,
    /// Failover for every rule that does not set its own.
    #[serde(default)]
    pub failover: FailoverPolicy,
    #[serde(default)]
    pub status: Status,
}

impl Doc for RoutingScheme {
    const KIND: &'static str = "routing_scheme";
    const PREFIX: &'static str = "rsc";
}

/// Where the scheme that applies to a template was assigned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RouteSource {
    Template,
    Workspace,
    Organisation,
    /// No scheme: the whole pool by priority.
    BuiltIn,
}

impl RouteSource {
    pub fn name(self) -> &'static str {
        match self {
            Self::Template => "template",
            Self::Workspace => "workspace",
            Self::Organisation => "organisation",
            Self::BuiltIn => "built_in",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_scheme_needs_only_a_name() {
        let s: RoutingScheme = serde_json::from_value(json!({ "name": "by-complexity" })).unwrap();
        assert!(s.rules.is_empty());
        assert_eq!(s.default.algorithm, Algorithm::Priority);
        assert_eq!(s.failover.max_targets, 3);
        assert!(s.failover.on.rate_limited && !s.failover.on.timeout);
    }

    #[test]
    fn rules_read_as_written() {
        let s: RoutingScheme = serde_json::from_value(json!({
            "name": "tiers",
            "rules": [
                { "name": "cheap", "when": { "complexity": ["low", "unset"], "agent_id": { "prefix": ["triage-"] } },
                  "then": { "tiers": ["small"], "algorithm": "weighted" } },
                { "name": "review", "when": { "task": { "in": ["code-review"] }, "tags": { "team": { "present": true } } },
                  "then": { "tiers": ["large", "small"], "overflow": "rest_of_pool",
                            "failover": { "max_targets": 2, "on": { "timeout": true } } } }
            ]
        }))
        .unwrap();
        assert_eq!(s.rules[0].when.complexity, vec![ComplexityMatch::Low, ComplexityMatch::Unset]);
        assert_eq!(s.rules[0].when.agent_id.prefix, vec!["triage-".to_owned()]);
        let f = s.rules[1].then.failover.unwrap();
        assert!(f.on.timeout && f.on.server_error, "unnamed switches keep their defaults");
        // Conditions that are not given are not written back.
        let back = serde_json::to_value(&s.rules[0].when).unwrap();
        assert_eq!(back, json!({ "complexity": ["low", "unset"], "agent_id": { "prefix": ["triage-"] } }));
    }

    #[test]
    fn unknown_fields_are_refused() {
        let r = serde_json::from_value::<RouteMatch>(json!({ "agent": { "in": ["x"] } }));
        assert!(r.is_err());
    }
}
