//! Plugins: installed for the platform, bound to organisations and workspaces
//! (`docs/plugin-concept.md`, section 4).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::Doc;

/// Turns a compiled plugin on for the installation. One per plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PluginInstall {
    /// The plugin's id, for example `intent`. `GET /plugins` lists the compiled ones.
    pub plugin: String,
    /// Off switches the plugin off everywhere; its bindings are kept.
    #[serde(default = "yes")]
    pub enabled: bool,
    /// The engines bindings may use. Empty allows all of the plugin's engines.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub engines: Vec<String>,
    /// The third parties engines may send text to, approved by name (for example `typesafe`).
    /// An engine whose third party is not listed cannot be bound.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub third_parties: Vec<String>,
    /// The platform's binding: it applies to every workspace whose organisation and own have no
    /// binding for the plugin. `locked` has no meaning here. Only for plugins whose settings
    /// name nothing of an organisation (not `intent`, whose bindings name a catalogue).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_binding: Option<PluginBinding>,
}

fn yes() -> bool {
    true
}

impl Doc for PluginInstall {
    const KIND: &'static str = "plugin_install";
    const PREFIX: &'static str = "pin";
}

/// What a binding does with calls.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PluginMode {
    /// Switched off; at a workspace, also switches an organisation's binding off for it.
    Off,
    /// Looks at calls alongside the provider call and records what it would have done. Never
    /// changes how a call is decided.
    #[default]
    Shadow,
    /// Looks at calls before they are planned; its verdict blocks, routes and limits them.
    Enforce,
}

impl PluginMode {
    pub fn name(self) -> &'static str {
        match self {
            PluginMode::Off => "off",
            PluginMode::Shadow => "shadow",
            PluginMode::Enforce => "enforce",
        }
    }
}

/// What an enforcing binding does with a call when the plugin gives no verdict.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OnError {
    /// The call is refused (feature catalogue GRD-06).
    #[default]
    Closed,
    /// The call proceeds without the plugin's signals.
    Open,
}

/// A plugin switched on for an organisation or a workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PluginBinding {
    /// The plugin's id. One binding per plugin in each organisation and workspace.
    pub plugin: String,
    #[serde(default)]
    pub mode: PluginMode,
    /// Among several plugins that block a call, the lowest gives the message.
    #[serde(default = "default_priority")]
    pub priority: u32,
    /// How long a call waits for the plugin. Unset takes the plugin's default for its engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u32>,
    #[serde(default)]
    pub on_error: OnError,
    /// Tell the caller the intent (`x-gw-intent`). Off: labels describe the catalogue.
    #[serde(default)]
    pub expose: bool,
    /// Organisations only: its workspaces cannot have a binding of their own for the plugin.
    #[serde(default)]
    pub locked: bool,
    /// The settings of the plugin the binding names. `plugin` in them names it again; it may
    /// be left out when writing, and must then match.
    #[serde(default)]
    #[schemars(with = "BindingSettings")]
    pub settings: Value,
}

/// The settings of a binding as the API describes them: those of the plugin named in `plugin`.
/// The schema only: settings are stored untagged, as the plugin reads them.
#[derive(JsonSchema)]
#[serde(tag = "plugin", rename_all = "snake_case")]
#[allow(dead_code)]
enum BindingSettings {
    Intent(pacod_gw_plugin_intent::Settings),
    ResponseCache(pacod_gw_plugin_cache::Settings),
}

impl PluginBinding {
    /// Takes the `plugin` tag out of the settings, as they are stored. A tag that names another
    /// plugin than the binding is refused.
    pub fn untag_settings(&mut self) -> Result<(), String> {
        let Some(obj) = self.settings.as_object_mut() else { return Ok(()) };
        match obj.shift_remove("plugin") {
            None => Ok(()),
            Some(Value::String(p)) if p == self.plugin => Ok(()),
            Some(other) => {
                Err(format!("settings.plugin is {other}, but the binding is of the plugin \"{}\"", self.plugin))
            }
        }
    }

    /// The binding as the API shows it: its settings carry the plugin's id in `plugin`.
    pub fn tagged(mut self) -> Self {
        let mut settings = match std::mem::take(&mut self.settings) {
            Value::Object(o) => o,
            _ => serde_json::Map::new(),
        };
        settings.insert("plugin".into(), Value::String(self.plugin.clone()));
        self.settings = Value::Object(settings);
        self
    }
}

fn default_priority() -> u32 {
    100
}

/// A plugin bound to an organisation: it applies to every workspace that has no binding of its
/// own for the plugin.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct OrgPluginBinding(pub PluginBinding);

impl Doc for OrgPluginBinding {
    const KIND: &'static str = "org_plugin_binding";
    const PREFIX: &'static str = "opb";
}

/// A plugin bound to a workspace. It replaces its organisation's binding for that workspace.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct WorkspacePluginBinding(pub PluginBinding);

impl Doc for WorkspacePluginBinding {
    const KIND: &'static str = "plugin_binding";
    const PREFIX: &'static str = "pbd";
}

pub use pacod_gw_plugin_intent::Catalogue;

/// An organisation's intent catalogue (`docs/plugin-concept.md`, section 5.2): its workspaces'
/// bindings may name it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct OrgIntentCatalogue(pub Catalogue);

impl Doc for OrgIntentCatalogue {
    const KIND: &'static str = "org_intent_catalogue";
    const PREFIX: &'static str = "oic";
}

/// A workspace's intent catalogue. It hides an organisation's catalogue of the same name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct WorkspaceIntentCatalogue(pub Catalogue);

impl Doc for WorkspaceIntentCatalogue {
    const KIND: &'static str = "intent_catalogue";
    const PREFIX: &'static str = "icg";
}

/// When the response cache's answers were cleared for a scope: the platform (no parent), an
/// organisation or a workspace (its parent). Answers asked for before then are misses
/// (`plugins/answers.rs`). One per scope, at the id [`ResponseCacheClear::id_for`] gives, so that
/// every instance learns of a clear through the configuration it polls.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ResponseCacheClear {
    /// Never moves backwards: a later clear keeps the later of the two times.
    pub cleared_at: chrono::DateTime<chrono::Utc>,
    /// The principal that cleared.
    pub by: String,
    #[serde(default)]
    pub by_kind: String,
}

impl Doc for ResponseCacheClear {
    const KIND: &'static str = "response_cache_clear";
    const PREFIX: &'static str = "rcc";
}

impl ResponseCacheClear {
    /// The id of a scope's clear: `rcc_platform`, or `rcc_` and the organisation's or
    /// workspace's id.
    pub fn id_for(scope_id: Option<&str>) -> String {
        format!("{}_{}", Self::PREFIX, scope_id.unwrap_or("platform"))
    }
}
