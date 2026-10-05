//! Workspace templates: a named, preconfigured LLM for one purpose (concept section 7).

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

/// The configuration a template version holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct TemplateSpec {
    /// The model: its id (`anthropic/claude-sonnet-5-5`) or object id.
    pub model: String,
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
