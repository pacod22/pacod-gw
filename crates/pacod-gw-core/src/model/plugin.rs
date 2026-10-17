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
    /// The plugin's settings. `GET /plugins` gives each plugin's schema.
    #[serde(default)]
    #[schemars(with = "PluginSettingsSchema")]
    pub settings: Value,
}

/// The settings of a binding: those of the plugin it names.
#[derive(JsonSchema)]
#[schemars(rename = "PluginSettings")]
#[allow(dead_code)]
struct PluginSettingsSchema(serde_json::Map<String, Value>);

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
