//! The contract between pacod-gw and its plugins (`docs/plugin-concept.md`, section 3).
//!
//! A plugin is compiled into the gateway and switched on by configuration. It reads a call
//! before the gateway plans it, attaches signals to it (a label, a probability, a flag, a
//! score) and may stop it. A plugin that answers calls (a response cache, section 15) may
//! instead answer a planned call with an answer the gateway kept. No plugin changes a call's
//! content.
//!
//! A plugin never reaches into the core. What it needs from the gateway, such as a model call,
//! it asks for through the [`Host`] the core hands it. That keeps plugins testable on their own
//! and is the contract an external plugin would later speak.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

pub use futures_util::future::BoxFuture;
pub use pacod_gw_dialects::canonical;
pub use pacod_gw_dialects::extract::{Conversation, Dialect, Role, Turn, conversation};
use serde::Serialize;
use serde_json::Value;

// --- what a plugin is ---------------------------------------------------------------------------

/// Where an engine sends the text it classifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Egress {
    /// Nowhere: the plugin works on its own.
    None,
    /// To the provider of a model the workspace may use.
    Provider,
    /// To a third party, which a platform administrator must approve by name.
    ThirdParty(&'static str),
}

/// One way a plugin can do its work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Engine {
    pub id: &'static str,
    pub egress: Egress,
}

/// Where in a call a plugin works (`docs/plugin-concept.md`, section 3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hook {
    /// Reads a call before it is planned, attaches signals and may block it
    /// ([`Bound::on_request`]).
    OnRequest,
    /// Answers a planned call from what was kept, and keeps answers ([`Bound::answers`]).
    Answers,
}

impl Hook {
    pub fn name(self) -> &'static str {
        match self {
            Hook::OnRequest => "on_request",
            Hook::Answers => "answers",
        }
    }
}

/// What a plugin is and what it does with data.
#[derive(Debug, Clone, Copy)]
pub struct Manifest {
    /// The id bindings name, for example `intent`.
    pub id: &'static str,
    /// The version of the plugin's behaviour, recorded with each verdict in audit records.
    pub version: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub engines: &'static [Engine],
    pub hooks: &'static [Hook],
    /// The gateway installs the plugin for the platform once, on its first start with it, with
    /// a default binding of [`Plugin::default_settings`] for every workspace.
    pub installed_by_default: bool,
}

impl Manifest {
    pub fn has(&self, hook: Hook) -> bool {
        self.hooks.contains(&hook)
    }

    /// The third parties an engine sends text to, if any.
    pub fn third_party(&self, engine: &str) -> Option<&'static str> {
        self.engines.iter().find(|e| e.id == engine).and_then(|e| match e.egress {
            Egress::ThirdParty(name) => Some(name),
            _ => None,
        })
    }
}

/// A catalogue a binding's settings name, as the core found it.
#[derive(Debug, Clone, Copy)]
pub struct Catalogued<'a> {
    pub id: &'a str,
    /// Raised on every change, so a changed catalogue is never answered from a cache.
    pub version: i32,
    pub doc: &'a Value,
}

/// What a plugin wants a model for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelUse {
    /// A chat model with JSON-schema output, or a `@template`.
    StructuredChat,
    /// A decision model (TypeSafe's System One API).
    Decision,
}

/// What a plugin is given to prepare a binding, once per configuration snapshot.
#[derive(Clone, Copy)]
pub struct CompileCtx<'a> {
    pub settings: &'a Value,
    /// The catalogue named by the settings' `catalogue`: the workspace's own, else its
    /// organisation's. `None` when the settings name none or it does not exist.
    pub catalogue: Option<Catalogued<'a>>,
    pub organisation_id: &'a str,
    /// Empty for an organisation's binding checked on its own.
    pub workspace_id: &'a str,
    /// Whether the binding's scope may use a model (by its id) or a `@template` for this use,
    /// and why not.
    pub check_model: &'a dyn Fn(&str, ModelUse) -> Result<(), String>,
}

/// A plugin, as compiled in. The registry holds one value of it for the life of the process,
/// so state that should outlive configuration changes (a cache) lives here.
pub trait Plugin: Send + Sync {
    fn manifest(&self) -> &'static Manifest;

    /// The engines a binding's settings use, for the check of third parties. By default all
    /// the plugin has.
    fn engines(&self, _settings: &Value) -> Vec<&'static str> {
        self.manifest().engines.iter().map(|e| e.id).collect()
    }

    /// How long a call waits for the plugin when its binding sets no time, by the settings
    /// (an engine that calls a large model needs longer than one that does not).
    fn default_timeout_ms(&self, _settings: &Value) -> u32 {
        2_000
    }

    /// Checks a binding's settings and builds what each call needs. Problems are refused when
    /// the binding is written, never on the request path.
    fn compile(&self, ctx: &CompileCtx<'_>) -> Result<Arc<dyn Bound>, Vec<String>>;

    /// The settings of the binding a default install gives every workspace
    /// ([`Manifest::installed_by_default`]).
    fn default_settings(&self) -> Option<Value> {
        None
    }
}

/// A plugin as one workspace has it switched on.
pub trait Bound: Send + Sync {
    /// Looks at a call before the gateway plans it. The core bounds the time it may take.
    /// Plugins without [`Hook::OnRequest`] are never asked.
    fn on_request<'a>(
        &'a self,
        _input: &'a RequestInput,
        _host: &'a dyn Host,
    ) -> BoxFuture<'a, Result<Verdict, PluginError>> {
        Box::pin(async { Ok(Verdict::allow(Signals::default())) })
    }

    /// Whether a planned call may be answered from a kept answer, and whether its own answer
    /// is kept: `None` when neither. Plugins without [`Hook::Answers`] are never asked. Pure:
    /// the core reads and writes the store.
    fn answers(&self, _call: &PlannedCall<'_>) -> Option<Keep> {
        None
    }

    /// Whether a complete answer is worth keeping. The core has already checked that the call
    /// succeeded and that the answer fits [`Keep::max_bytes`].
    fn keeps(&self, _answer: &canonical::Response) -> bool {
        false
    }
}

// --- what an answering plugin sees ------------------------------------------------------------

/// What a caller asks of a response cache for one call (`x-gw-cache`, or the body's
/// `gateway.cache.response`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CacheDirective {
    /// As the binding says.
    #[default]
    Default,
    /// Neither answered from the cache nor kept in it.
    Off,
    /// Not answered from the cache; the fresh answer is kept.
    Refresh,
}

/// A call as the gateway planned it: the model it goes to and the request that model is sent.
#[derive(Debug, Clone, Copy)]
pub struct PlannedCall<'a> {
    pub workspace_id: &'a str,
    /// The repository's object id of the model that serves the call.
    pub model_id: &'a str,
    /// The model's id at its provider.
    pub upstream_id: &'a str,
    /// The provider's object id.
    pub provider_id: &'a str,
    /// The template the call named, and its version.
    pub template: Option<(&'a str, u32)>,
    /// The dialect the provider is spoken to in: `OpenAiChat`, `Anthropic` or `Gemini`.
    pub dialect: Dialect,
    /// The request as the provider receives it.
    pub body: &'a [u8],
    /// Request headers passed to the provider that change its answer, by lower-case name.
    pub headers: &'a [(&'a str, &'a str)],
    pub directive: CacheDirective,
    /// The temperature the caller asked for in its own request. A model that takes no
    /// temperature (most that reason) is sent none, so `body` does not show it.
    pub asked_temperature: Option<f64>,
}

/// How a planned call is answered from kept answers, and how its own answer is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keep {
    /// Identifies the request within the workspace. The core keeps answers per workspace.
    pub key: [u8; 32],
    /// How long an answer is kept.
    pub ttl: Duration,
    /// Answer the call from a kept answer when there is one.
    pub lookup: bool,
    /// Keep the call's answer.
    pub store: bool,
    /// Larger answers, as stored, are not kept.
    pub max_bytes: usize,
}

// --- what a plugin sees ------------------------------------------------------------------------

/// What the caller said about the call, besides its content (the `x-gw-*` headers and the
/// body's `gateway` object).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallParams {
    pub task: Option<String>,
    pub complexity: Option<String>,
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
    pub end_user: Option<String>,
    pub session_id: Option<String>,
    /// Tags of keys the workspace declared.
    pub tags: BTreeMap<String, String>,
}

/// What a plugin sees of a call. Never the raw body, the headers or a credential.
#[derive(Debug, Clone, Default)]
pub struct RequestInput {
    /// The call's request id (`gw_…`).
    pub call_id: String,
    pub organisation_id: String,
    pub workspace_id: String,
    pub key_id: String,
    pub key_name: String,
    /// `openai`, `anthropic`, `gemini`, `native` or `proxy`.
    pub interface: String,
    /// Proxy mode: the provider in the URL.
    pub provider: Option<String>,
    pub path: String,
    /// The model or `@template` the caller named.
    pub model: String,
    pub stream: bool,
    pub body_bytes: usize,
    pub params: CallParams,
    pub conversation: Conversation,
}

// --- what a plugin answers ---------------------------------------------------------------------

/// What an intent classification found.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IntentSignal {
    /// The label, or `unknown`.
    pub label: String,
    /// From 0 to 1.
    pub confidence: f32,
    /// The label routing and limits act on: `label` when its confidence reaches the binding's
    /// threshold, `unknown` below it.
    pub act: String,
    /// The engine that answered, for example `jev` or `cascade:llm`.
    pub engine: String,
    /// Answered from the cache.
    pub cached: bool,
}

/// The signals a plugin attaches to a call. Routing, limits, the usage event and the audit
/// record read them.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Signals {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub intent: Option<IntentSignal>,
    /// Probability of yes, by question id.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub flags: BTreeMap<String, f32>,
    /// The score, by question id.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub scores: BTreeMap<String, f32>,
}

impl Signals {
    pub fn is_empty(&self) -> bool {
        self.intent.is_none() && self.flags.is_empty() && self.scores.is_empty()
    }

    /// The intent label routing and limits act on.
    pub fn act(&self) -> Option<&str> {
        self.intent.as_ref().map(|i| i.act.as_str())
    }
}

/// What happens to the call.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum Action {
    Allow,
    /// The call is refused with `403` and this error type, in the caller's dialect.
    Block {
        code: &'static str,
        message: String,
    },
}

/// What a rule in trial would have done.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TrialAction {
    Block,
    Flag,
}

/// A rule in trial (`shadow`) that matched: recorded, never acted on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Trial {
    pub rule: String,
    pub action: TrialAction,
}

/// A plugin's answer about one call.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Verdict {
    pub signals: Signals,
    pub action: Action,
    /// A rule asked for the call to be marked: its audit record is captured.
    pub flag: bool,
    /// The rules that matched and asked for an alert.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub alerts: Vec<String>,
    /// The rule that decided, if one did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub trial: Vec<Trial>,
    /// For the audit record only: probabilities, a rationale, the versions that answered. It
    /// can quote the content, so it never goes into usage events or traces.
    #[serde(skip_serializing_if = "Value::is_null")]
    pub detail: Value,
}

impl Verdict {
    pub fn allow(signals: Signals) -> Self {
        Self {
            signals,
            action: Action::Allow,
            flag: false,
            alerts: Vec::new(),
            rule: None,
            trial: Vec::new(),
            detail: Value::Null,
        }
    }
}

// --- errors ----------------------------------------------------------------------------------------

/// A limit of the workspace refused a call the plugin made. The call itself would hit it too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refused {
    /// `429` or `402`.
    pub status: u16,
    /// The gateway's error type, `rate_limited` or `quota_exceeded`.
    pub kind: String,
    pub message: String,
    pub retry_after_seconds: Option<u32>,
}

/// Why a plugin gave no verdict (`docs/plugin-concept.md`, section 3.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorKind {
    /// No answer within the binding's time.
    Timeout,
    /// The engine's provider failed.
    Upstream,
    /// The workspace's own limits refused the engine's call.
    Limited(Refused),
    /// The answer did not parse, or named a label that is not in the catalogue.
    InvalidAnswer,
    /// The workspace has no credential for the engine's provider, or the engine's model is
    /// gone.
    NoCredential,
    /// This instance's binary lacks the plugin, or its binding no longer compiles.
    NotBuilt,
}

impl ErrorKind {
    /// The value of the usage event's `plugin_error`.
    pub fn name(&self) -> &'static str {
        match self {
            ErrorKind::Timeout => "timeout",
            ErrorKind::Upstream => "upstream",
            ErrorKind::Limited(_) => "limited",
            ErrorKind::InvalidAnswer => "invalid_answer",
            ErrorKind::NoCredential => "no_credential",
            ErrorKind::NotBuilt => "not_built",
        }
    }

    /// Whether the fault is in the configuration: retrying does not clear it.
    pub fn is_configuration(&self) -> bool {
        matches!(self, ErrorKind::NoCredential | ErrorKind::NotBuilt)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginError {
    pub kind: ErrorKind,
    pub message: String,
}

impl PluginError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self { kind, message: message.into() }
    }
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.name(), self.message)
    }
}

/// What the host could not do for a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    Limited(Refused),
    /// The provider failed, or the gateway could not reach it.
    Upstream(String),
    /// No credential for the provider, or the model is unknown or not usable.
    Unavailable(String),
    /// The gateway refused the request the plugin built: a defect of the plugin.
    Invalid(String),
}

impl From<HostError> for PluginError {
    fn from(e: HostError) -> Self {
        match e {
            HostError::Limited(r) => {
                let m = r.message.clone();
                PluginError::new(ErrorKind::Limited(r), m)
            }
            HostError::Upstream(m) => PluginError::new(ErrorKind::Upstream, m),
            HostError::Unavailable(m) => PluginError::new(ErrorKind::NoCredential, m),
            HostError::Invalid(m) => PluginError::new(ErrorKind::InvalidAnswer, m),
        }
    }
}

// --- the host ----------------------------------------------------------------------------------

/// What the core does for a plugin. Its calls are the gateway's own: accounted, limited and
/// priced in the classified call's workspace, and never classified themselves.
pub trait Host: Send + Sync {
    /// A chat call on the native interface (`/gw/v1/chat`), through the managed path. Returns
    /// the native answer.
    fn complete<'a>(&'a self, request: Value) -> BoxFuture<'a, Result<Value, HostError>>;

    /// A decision call (TypeSafe's System One API) on a decision model of the repository, by
    /// the model's id (`typesafe/jev-1.13.0`). `request` carries `state` and `questions`; the
    /// host sets the provider's model id. Returns the provider's answer.
    fn decide<'a>(&'a self, model: &'a str, request: Value) -> BoxFuture<'a, Result<Value, HostError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_errors_map_to_plugin_errors() {
        let limited =
            Refused { status: 429, kind: "rate_limited".into(), message: "slow".into(), retry_after_seconds: Some(3) };
        let e: PluginError = HostError::Limited(limited.clone()).into();
        assert_eq!(e.kind, ErrorKind::Limited(limited));
        assert_eq!(PluginError::from(HostError::Unavailable("no key".into())).kind.name(), "no_credential");
        assert!(ErrorKind::NotBuilt.is_configuration() && !ErrorKind::Timeout.is_configuration());
    }

    #[test]
    fn a_verdict_serialises_without_empty_parts() {
        let v = serde_json::to_value(Verdict::allow(Signals::default())).unwrap();
        assert_eq!(v, serde_json::json!({ "signals": {}, "action": { "action": "allow" }, "flag": false }));
    }
}
