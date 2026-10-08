//! Organisations, workspaces and API keys (concept section 5).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::routing::{Affinity, Algorithm, RouteAction, RoutingScheme};
use super::settings::{AuditAttachments, AuditMode, AuditSettings, ScopeSettings, UnsupportedParams};
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

/// A starting configuration for an organisation, which its administrators load in the
/// console: the settings its workspaces inherit and the routing schemes they name. It holds
/// only what the organisation's own administrators may set; model grants and limits name
/// the installation's models and are the platform's to decide.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct OrgConfigTemplate {
    /// Set on an organisation where it has no value of its own.
    pub settings: ScopeSettings,
    /// Created where the organisation has no scheme of the same name.
    pub routing_schemes: Vec<RoutingScheme>,
    /// Why each setting has its value, by the setting's name.
    pub reasons: BTreeMap<String, String>,
}

/// The built-in organisation template. Cautious values: calls are recorded only when they
/// ask for it, nothing is locked, and every workspace may still choose otherwise.
pub fn org_config_template() -> OrgConfigTemplate {
    let scheme = |name: &str, description: &str, default: RouteAction| RoutingScheme {
        name: name.into(),
        description: description.into(),
        rules: Vec::new(),
        default,
        failover: Default::default(),
        status: Status::Active,
    };
    let reasons = [
        (
            "unsupported_params",
            "Calls keep working when a model does not take a parameter: it is removed, and the response says what was removed.",
        ),
        (
            "audit",
            "Calls are recorded in full only when they ask for it with the x-gw-audit header, for debugging. Attachments are kept as size and hash only, records for 14 days.",
        ),
        (
            "routing_scheme",
            "Templates with several models use the scheme standard unless they or their workspace name another: one place to change routing for the whole organisation.",
        ),
    ];
    OrgConfigTemplate {
        settings: ScopeSettings {
            unsupported_params: Some(UnsupportedParams::Drop),
            audit: Some(AuditSettings {
                mode: AuditMode::OnRequest,
                retention_days: Some(14),
                attachments: AuditAttachments::Reference,
                ..Default::default()
            }),
            routing_scheme: Some("standard".into()),
            ..Default::default()
        },
        routing_schemes: vec![
            scheme(
                "standard",
                "Main and backups: the pool's models by priority; the next one takes over on a 429, a 5xx or a lost connection. Add rules here to route by complexity, task or tags.",
                RouteAction::default(),
            ),
            scheme(
                "spread-load",
                "Spreads calls over the pool by weight and keeps each session (x-gw-session) on one model, which keeps prompt caches warm.",
                RouteAction { algorithm: Algorithm::Weighted, affinity: Affinity::Session, ..Default::default() },
            ),
        ],
        reasons: reasons.into_iter().map(|(k, v)| (k.to_owned(), v.to_owned())).collect(),
    }
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
    /// The most calls this key may have in progress at once, across all gateway instances:
    /// each of N running gateways allows `ceil(max / N)`.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_organisation_template_holds_together() {
        let t = org_config_template();
        // The schemes compile as a POST would, and have distinct names.
        for s in &t.routing_schemes {
            crate::routing::compile("org_test", 0, s).unwrap_or_else(|e| panic!("{}: {e}", s.name));
        }
        let mut names: Vec<_> = t.routing_schemes.iter().map(|s| s.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), t.routing_schemes.len());
        // The scheme the settings assign comes with the template.
        let assigned = t.settings.routing_scheme.as_deref().unwrap();
        assert!(names.contains(&assigned));
        // Every setting it sets has a reason, and it locks nothing.
        let set = serde_json::to_value(&t.settings).unwrap();
        let set: std::collections::BTreeSet<_> = set.as_object().unwrap().keys().cloned().collect();
        assert_eq!(set, t.reasons.keys().cloned().collect());
        assert!(t.settings.locked.is_empty());
    }
}
