//! Workspace templates: a named, preconfigured LLM for one purpose (concept section 7).

use std::borrow::Cow;
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::settings::UnsupportedParams;
use super::{Doc, Status};

/// One parameter of a template.
///
/// Left out entirely, the template has no opinion and the caller's value (or the model's
/// default) applies. With `omit`, the parameter is never sent. With a `value`, that is the
/// default the caller may override, or with `locked` the value that always applies.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Param<T> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    /// The caller cannot override the value.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub locked: bool,
    /// Never send this parameter, even when the caller does.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub omit: bool,
}

impl<T> Param<T> {
    pub fn default_value(v: T) -> Self {
        Self { value: Some(v), locked: false, omit: false }
    }

    pub fn locked(v: T) -> Self {
        Self { value: Some(v), locked: true, omit: false }
    }

    pub fn omitted() -> Self {
        Self { value: None, locked: false, omit: true }
    }
}

/// What a template says about one parameter, resolved from an optional [`Param`].
#[derive(Debug, Clone, PartialEq)]
pub enum Rule<T> {
    /// No opinion.
    Unset,
    Omit,
    Default(T),
    Locked(T),
}

impl<T: Clone> Rule<T> {
    pub fn of(p: &Option<Param<T>>) -> Self {
        match p {
            None => Rule::Unset,
            Some(p) if p.omit => Rule::Omit,
            Some(Param { value: Some(v), locked: true, .. }) => Rule::Locked(v.clone()),
            Some(Param { value: Some(v), .. }) => Rule::Default(v.clone()),
            Some(_) => Rule::Unset,
        }
    }
}

/// How the caller's prompt-cache markers are treated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheMode {
    /// Remove the caller's markers.
    Off,
    /// Keep the caller's markers.
    #[default]
    Passthrough,
    /// Let the provider place the marker (Anthropic models).
    Auto,
}

/// Audit mode for calls using a template (concept section 14.6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditSwitch {
    /// Follow the workspace.
    #[default]
    Inherit,
    On,
    Off,
}

/// How a template acts in proxy mode (concept section 7.4).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProxyBehaviour {
    /// Write the template's model and parameters into the request.
    #[default]
    Apply,
    /// Write nothing; reject requests that do not already conform.
    Check,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ReasoningSpec {
    /// The effort level, for example `low`, `high`, or `none` to switch reasoning off.
    pub effort: Option<Param<String>>,
    /// Budget-style models: the thinking budget in tokens.
    pub budget_tokens: Option<Param<u32>>,
    /// Return a summary of the reasoning.
    pub summary: Option<Param<bool>>,
}

/// The most models one template may hold.
pub const MAX_POOL: usize = 16;

fn one() -> u32 {
    1
}

fn is_one(w: &u32) -> bool {
    *w == 1
}

fn is_zero(p: &u8) -> bool {
    *p == 0
}

/// One model of a template's pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PoolEntry {
    /// The model: its id or object id.
    pub model: String,
    /// A label routing schemes choose by, for example `small` or `large`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tier: Option<String>,
    /// The share of calls for the weighted algorithms. 0: only ever a backup.
    #[serde(default = "one", skip_serializing_if = "is_one")]
    pub weight: u32,
    /// Main and backups: lower comes first.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub priority: u8,
}

impl PoolEntry {
    pub fn of(model: &str) -> Self {
        Self { model: model.to_owned(), tier: None, weight: 1, priority: 0 }
    }
}

/// The configuration a template version holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TemplateSpec {
    /// The model: its id (`anthropic/claude-sonnet-5-5`) or object id. Empty when the template
    /// has a pool of models in `models` instead.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub model: String,
    /// Several models, chosen per call by the routing scheme. Instead of `model`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<PoolEntry>,
    /// The routing scheme of the organisation that chooses among `models`. Unset: the
    /// workspace's, else the organisation's, else the pool in order of priority.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing_scheme: Option<String>,
    /// More members from pacod-broker's catalogue, after the template's own models (runbook
    /// section 20). The template's own models stay first: a pool from the broker adds, it never
    /// replaces.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pool_from: Option<PoolFrom>,
    /// Offer this template on pacod-broker: gw-broker announces it under the organisation's
    /// broker token (runbook section 20).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub broker_offer: Option<BrokerOffer>,
    /// The default output-token limit, and the most a caller may ask for.
    pub max_output_tokens: Option<u32>,
    pub temperature: Option<Param<f64>>,
    pub top_p: Option<Param<f64>>,
    pub frequency_penalty: Option<Param<f64>>,
    pub presence_penalty: Option<Param<f64>>,
    pub reasoning: ReasoningSpec,
    /// How long to wait for the provider, up to 600. For streams: until the first byte.
    pub timeout_seconds: Option<u32>,
    /// Attempts before the first response byte. 1 means no retry.
    pub retry_max_attempts: Option<u8>,
    pub prompt_cache: PromptCacheMode,
    /// Overrides the workspace's policy for parameters the model does not accept.
    pub unsupported_params: Option<UnsupportedParams>,
    pub audit: AuditSwitch,
    /// Tags added to every call, for example `purpose`.
    pub tags: BTreeMap<String, String>,
    pub proxy: ProxyBehaviour,
    /// Proxy mode: ask the provider to report usage on OpenAI-dialect streams.
    pub request_stream_usage: bool,
}

/// How a template is offered on pacod-broker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BrokerOffer {
    /// Who finds it: `organisation` (the organisation's own members, the default), `platform`
    /// (everyone signed in) or `public`.
    #[serde(default = "own_organisation")]
    pub visibility: String,
    /// One line for consumers; the template's description when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub summary: String,
}

fn own_organisation() -> String {
    "organisation".into()
}

impl BrokerOffer {
    /// The offer as gw-broker can announce it; the error names the field.
    pub fn check(&self) -> Result<(), String> {
        if !matches!(self.visibility.as_str(), "organisation" | "platform" | "public") {
            return Err("visibility: organisation, platform or public".into());
        }
        Ok(())
    }
}

/// The broker resource key a model is offered under (runbook section 20.1): `model:` and its
/// name in lower case. None when the name cannot be one: the broker takes a slug (a-z, 0-9,
/// `- _ . : /`) of at most 96 characters, and refuses an organisation's whole announcement
/// for one key that is not. Templates' keys are `<workspace>.<template>` and `org:<template>`;
/// neither slug has a colon, so no template's key is a model's.
pub fn broker_model_key(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    let slug = lower.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
        && lower.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_.:/".contains(&b));
    let key = format!("model:{lower}");
    (slug && key.len() <= 96).then_some(key)
}

/// A pool filled from pacod-broker's catalogue.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PoolFrom {
    /// A broker discovery query, for example `{"kind": "model", "regions": ["eu-central"],
    /// "max_price": {"input": 1}, "min_reputation": 0.6}`. Only resources that are models of the
    /// repository (`broker_resource_id`), active and granted to the template's workspace (or, for
    /// an organisation's template, to the organisation), can join.
    pub query: serde_json::Value,
    /// `propose` (the default): a change in the answer waits for an admin to accept it.
    /// `follow`: the pool follows the answer, each change in the audit log.
    #[serde(default)]
    pub mode: PoolMode,
    /// The most members it adds, 1 to 15.
    #[serde(default = "default_pool_max")]
    pub max: u8,
    /// Self-hosted endpoints of the template's own organisation that announce themselves to the
    /// broker join as they come and go: the core keeps a provider and a model for each. Needs
    /// `follow`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own_leases: bool,
}

fn default_pool_max() -> u8 {
    4
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PoolMode {
    #[default]
    Propose,
    Follow,
}

/// What a template's `pool_from` resolved to (kind `broker_pool`, one per template, in the
/// template's workspace, or in its organisation for an organisation's template). Written by the
/// core's round with the broker, read by the snapshot.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BrokerPool {
    pub template_id: String,
    /// The members in force, after the template's own models.
    pub current: Vec<PoolEntry>,
    /// What the broker answers now, when it differs and the mode is `propose`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub proposed: Option<Vec<PoolEntry>>,
    /// The broker resources of the members, by model name, for the console.
    pub resources: BTreeMap<String, String>,
    pub resolved_at: Option<DateTime<Utc>>,
    /// What could not be used, in words.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub problems: Vec<String>,
}

impl Doc for BrokerPool {
    const KIND: &'static str = "broker_pool";
    const PREFIX: &'static str = "bpl";
}

impl TemplateSpec {
    /// The models of the template: the pool, or the single model.
    pub fn pool(&self) -> Cow<'_, [PoolEntry]> {
        if self.models.is_empty() { Cow::Owned(vec![PoolEntry::of(&self.model)]) } else { Cow::Borrowed(&self.models) }
    }

    /// The pool with the members from the broker: after the template's own models, never in
    /// their place, and at most `MAX_POOL` in all.
    pub fn pool_with(&self, from_broker: &[PoolEntry]) -> Vec<PoolEntry> {
        let own = self.pool();
        let after = own.iter().map(|e| e.priority).max().unwrap_or(0).saturating_add(1);
        let extra = from_broker.iter().filter(|e| !own.iter().any(|o| o.model == e.model)).map(|e| {
            let mut e = e.clone();
            e.priority = e.priority.max(after);
            e
        });
        own.iter().cloned().chain(extra).take(MAX_POOL).collect()
    }

    /// The model listed for the template: the single model, or the first of the pool by
    /// priority.
    pub fn primary_model(&self) -> &str {
        match self.models.iter().filter(|e| e.weight > 0).min_by_key(|e| e.priority) {
            Some(e) if self.model.is_empty() => &e.model,
            _ => &self.model,
        }
    }
}

/// One saved version of a template.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TemplateVersion {
    pub version: u32,
    pub spec: TemplateSpec,
    #[serde(default)]
    pub comment: String,
    /// The principal that saved it.
    #[serde(default)]
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// How many versions of a template are kept.
pub const MAX_TEMPLATE_VERSIONS: usize = 50;

/// A template with its versions. Addressed as `@name`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Template {
    /// Slug, unique in the workspace.
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// The version calls use.
    pub current_version: u32,
    /// Oldest first. The current version is always among them.
    pub versions: Vec<TemplateVersion>,
    #[serde(default)]
    pub status: Status,
}

impl Doc for Template {
    const KIND: &'static str = "template";
    const PREFIX: &'static str = "tpl";
}

/// A template owned by an organisation. Every workspace of the organisation inherits it,
/// unless the workspace has a template of the same name of its own, which hides it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct OrgTemplate(pub Template);

impl Doc for OrgTemplate {
    const KIND: &'static str = "org_template";
    const PREFIX: &'static str = "otp";
}

impl Template {
    pub fn current(&self) -> Option<&TemplateVersion> {
        self.versions.iter().find(|v| v.version == self.current_version)
    }

    pub fn next_version(&self) -> u32 {
        self.versions.iter().map(|v| v.version).max().unwrap_or(0) + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parameter_rules() {
        let p =
            |v: serde_json::Value| -> Rule<f64> { Rule::of(&serde_json::from_value::<Option<Param<f64>>>(v).unwrap()) };
        assert_eq!(p(json!(null)), Rule::Unset);
        assert_eq!(p(json!({ "value": 0.3 })), Rule::Default(0.3));
        assert_eq!(p(json!({ "value": 0.3, "locked": true })), Rule::Locked(0.3));
        assert_eq!(p(json!({ "omit": true })), Rule::Omit);
        assert_eq!(p(json!({ "omit": true, "value": 0.3 })), Rule::Omit, "omit wins");
        assert_eq!(p(json!({})), Rule::Unset);
    }

    #[test]
    fn a_models_broker_key_is_a_slug_or_nothing() {
        assert_eq!(broker_model_key("openai/gpt-4o").as_deref(), Some("model:openai/gpt-4o"));
        assert_eq!(broker_model_key("Qwen/Qwen2.5-72B").as_deref(), Some("model:qwen/qwen2.5-72b"), "lower case");
        assert_eq!(broker_model_key("ollama/llama3:8b").as_deref(), Some("model:ollama/llama3:8b"));
        assert_eq!(broker_model_key("_private/x"), None, "a slug starts with a letter or digit");
        assert_eq!(broker_model_key(""), None);
        assert!(broker_model_key(&"a".repeat(90)).is_some(), "model: and 90 characters are 96");
        assert_eq!(broker_model_key(&"a".repeat(91)), None);
        let offer = BrokerOffer { visibility: "everyone".into(), summary: String::new() };
        assert!(offer.check().unwrap_err().starts_with("visibility"));
    }

    #[test]
    fn a_spec_needs_only_the_model() {
        let s: TemplateSpec = serde_json::from_value(json!({ "model": "anthropic/claude-haiku-4-5" })).unwrap();
        assert_eq!(s.prompt_cache, PromptCacheMode::Passthrough);
        assert_eq!(s.proxy, ProxyBehaviour::Apply);
        assert_eq!(s.audit, AuditSwitch::Inherit);
        assert!(s.temperature.is_none() && !s.request_stream_usage);
        // Unset parameters are not written back.
        let back = serde_json::to_value(Param::<f64>::default_value(0.5)).unwrap();
        assert_eq!(back, json!({ "value": 0.5 }));
    }

    #[test]
    fn a_single_model_spec_is_written_back_unchanged() {
        // Stored templates and export files predate pools; they must read and write the same.
        let old = json!({
            "model": "anthropic/claude-haiku-4-5", "max_output_tokens": null, "temperature": null, "top_p": null,
            "frequency_penalty": null, "presence_penalty": null,
            "reasoning": { "effort": null, "budget_tokens": null, "summary": null },
            "timeout_seconds": null, "retry_max_attempts": null, "prompt_cache": "passthrough",
            "unsupported_params": null, "audit": "inherit", "tags": {}, "proxy": "apply",
            "request_stream_usage": false
        });
        let s: TemplateSpec = serde_json::from_value(old.clone()).unwrap();
        assert_eq!(serde_json::to_value(&s).unwrap(), old);
        assert_eq!(&*s.pool(), &[PoolEntry::of("anthropic/claude-haiku-4-5")]);
        assert_eq!(s.primary_model(), "anthropic/claude-haiku-4-5");
    }

    #[test]
    fn a_pool_round_trips() {
        let s: TemplateSpec = serde_json::from_value(json!({
            "models": [
                { "model": "a/small", "tier": "small", "weight": 3, "priority": 1 },
                { "model": "a/large", "tier": "large" },
                { "model": "a/standby", "weight": 0 }
            ],
            "routing_scheme": "by-complexity"
        }))
        .unwrap();
        assert_eq!(s.pool().len(), 3);
        assert_eq!(s.models[1].weight, 1);
        assert_eq!(s.primary_model(), "a/large", "lowest priority among entries that take calls");
        let back = serde_json::to_value(&s).unwrap();
        assert!(back.get("model").is_none());
        assert_eq!(back["models"][1], json!({ "model": "a/large", "tier": "large" }));
        assert_eq!(serde_json::from_value::<TemplateSpec>(back).unwrap(), s);
    }

    #[test]
    fn versions() {
        let v = |n| TemplateVersion {
            version: n,
            spec: TemplateSpec::default(),
            comment: String::new(),
            created_by: String::new(),
            created_at: Utc::now(),
        };
        let t = Template {
            name: "planner".into(),
            description: String::new(),
            current_version: 2,
            versions: vec![v(1), v(2), v(3)],
            status: Status::Active,
        };
        assert_eq!(t.current().unwrap().version, 2);
        assert_eq!(t.next_version(), 4);
    }
}
