//! Organisations, workspaces and API keys (concept section 5).

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::settings::ScopeSettings;
use super::{Doc, Status};

/// A customer or a large internal unit. Groups workspaces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Organisation {
    /// Slug, unique in the installation.
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub settings: ScopeSettings,
}

impl Doc for Organisation {
    const KIND: &'static str = "organisation";
    const PREFIX: &'static str = "org";
}

/// The unit applications work in: one application, project, environment or team.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Workspace {
    /// Slug, unique in the organisation.
    pub name: String,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub status: Status,
    #[serde(default)]
    pub settings: ScopeSettings,
    /// The tag keys requests may carry. Undeclared keys are dropped. At most eight.
    #[serde(default)]
    pub tag_keys: Vec<String>,
    /// IANA time zone for calendar-aligned limit windows.
    #[serde(default = "default_tz")]
    pub time_zone: String,
}

fn default_tz() -> String {
    "UTC".into()
}

impl Doc for Workspace {
    const KIND: &'static str = "workspace";
    const PREFIX: &'static str = "ws";
}

/// A processing mode of the data plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    Proxy,
    Managed,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Proxy => "proxy",
            Mode::Managed => "managed",
        }
    }
}

/// What happens in proxy mode when the provider will not report usage (concept section 4.2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UsagePolicy {
    /// Reject before forwarding.
    Require,
    /// Forward and record an estimate.
    #[default]
    Estimate,
    /// Forward and count the request only.
    Ignore,
}

/// Whether proxy mode reads the request body.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InspectMode {
    /// Buffer and scan the body.
    #[default]
    Full,
    /// Blind relay: stream the body without reading it.
    None,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum KeyStatus {
    #[default]
    Active,
    Revoked,
}

/// A gateway API key. Belongs to one workspace. The secret itself is never stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApiKey {
    pub name: String,
    /// HMAC-SHA256 of the key, hex.
    pub secret_hmac: String,
    /// After a rotation: the previous key's hash, valid until `prev_valid_until`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_hmac: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prev_valid_until: Option<DateTime<Utc>>,
    /// The last four characters of the secret, to recognise a key in listings.
    #[serde(default)]
    pub hint: String,
    #[serde(default = "all_modes")]
    pub allowed_modes: Vec<Mode>,
    /// Model ids this key may use. Empty means every model the workspace may use.
    #[serde(default)]
    pub allowed_models: Vec<String>,
    /// Template names this key may use. Empty means every template of the workspace.
    #[serde(default)]
    pub allowed_templates: Vec<String>,
    /// Proxy mode: accept only requests that name a template.
    #[serde(default)]
    pub require_template: bool,
    #[serde(default)]
    pub usage_policy: UsagePolicy,
    #[serde(default)]
    pub inspect: InspectMode,
    /// The most calls this key may have in progress at once on one instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub status: KeyStatus,
}

pub fn all_modes() -> Vec<Mode> {
    vec![Mode::Proxy, Mode::Managed]
}

impl Doc for ApiKey {
    const KIND: &'static str = "api_key";
    const PREFIX: &'static str = "key";
}

impl ApiKey {
    /// Whether the key may be used at `now`.
    pub fn usable(&self, now: DateTime<Utc>) -> bool {
        self.status == KeyStatus::Active && self.expires_at.is_none_or(|t| now < t)
    }
}
