//! Settings and how they are inherited (concept section 5.3): the nearest defined value wins
//! (workspace, then organisation, then global), unless the organisation locked it.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::Doc;

/// What happens to a parameter the chosen model does not accept.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedParams {
    /// Remove it and report what was removed.
    #[default]
    Drop,
    /// Reject the request.
    Reject,
}

/// When calls of a workspace are recorded in full (concept section 14.6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditMode {
    #[default]
    Off,
    /// Every call.
    On,
    /// Only calls that ask for it (`gateway.audit` or the `x-gw-audit` header).
    OnRequest,
}

/// What a higher level decides about audit mode for everything below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditRule {
    /// Audit mode cannot be switched on, for privacy.
    Forbid,
    /// Every call is recorded, for compliance.
    Force,
}

/// How attachments (images, documents, audio) are kept in an audit record.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditAttachments {
    /// As sent.
    #[default]
    Full,
    /// Their size and hash only.
    Reference,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AuditGuarantee {
    /// A call proceeds even when its record cannot be stored; the loss is counted.
    #[default]
    BestEffort,
    /// A call is refused with `503 audit_unavailable` when its record cannot be stored.
    Required,
}

/// Audit mode of an organisation or a workspace.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct AuditSettings {
    pub mode: AuditMode,
    /// When audit mode switches itself off again. Suits a debugging session.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
    /// The share of calls recorded, above 0 and up to 1. Default 1.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample: Option<f64>,
    /// How long records are kept. Default 14 days.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_days: Option<u32>,
    pub attachments: AuditAttachments,
    pub guarantee: AuditGuarantee,
    /// Organisations only: rule audit mode out, or make it mandatory, for every workspace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rule: Option<AuditRule>,
}

/// How routing sets aside a model that keeps failing (its circuit breaker). Applies to every
/// gateway instance; each keeps its own count.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct BreakerSettings {
    /// Failures in a row (errors, no connection, no answer in time) that set a model aside.
    pub failures: u32,
    /// How long a model is set aside the first time. Each further time without a success in
    /// between doubles it, up to `max_cooldown_seconds`.
    pub cooldown_seconds: u32,
    pub max_cooldown_seconds: u32,
}

impl Default for BreakerSettings {
    fn default() -> Self {
        Self { failures: 5, cooldown_seconds: 30, max_cooldown_seconds: 300 }
    }
}

/// Installation-wide defaults. A single object with the id `settings_global`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct GlobalSettings {
    pub unsupported_params: UnsupportedParams,
    /// The currency reports are shown in.
    pub reporting_currency: String,
    /// Session lifetime of console logins, in hours.
    pub session_hours: u32,
    /// Session lifetime when "Keep me logged in" is ticked at login, in days.
    pub remember_me_days: u32,
    /// Users must confirm their email address before they can log in. Applies only when the
    /// installation can send email; accounts created without email need no confirmation.
    pub require_verified_email: bool,
    /// How long an email-verification link stays valid, in hours.
    pub verification_link_hours: u32,
    /// How long a password-reset link stays valid, in minutes.
    pub reset_link_minutes: u32,
    /// How long an invitation to an organisation or a workspace stays valid, in days.
    pub invitation_link_days: u32,
    /// Rules audit mode out, or makes it mandatory, for the whole installation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_rule: Option<AuditRule>,
    /// The most one body of an audit record may hold. A larger body is stored up to this
    /// size and marked as truncated.
    pub audit_max_body_bytes: u64,
    /// Routing among the models of a template: when a failing model is set aside.
    pub breaker: BreakerSettings,
    /// The Anthropic beta features (`anthropic-beta` values) calls may ask for. Unset allows
    /// all of them; an empty list allows none. Checked in both modes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub anthropic_betas_allowed: Option<Vec<String>>,
    /// The default for the provider tools calls may use, where neither the workspace nor its
    /// organisation sets one. Unset allows all of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_tools: Option<Vec<String>>,
    /// The default audit mode, where neither the workspace nor its organisation sets one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit: Option<AuditSettings>,
    /// How many days usage events are kept, where neither the workspace nor its organisation
    /// sets it. Unset or 0 keeps them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_retention_days: Option<u32>,
    /// The tag keys of workspaces that declare none, and whose organisation declares none.
    pub tag_keys: Vec<String>,
    /// How often model discovery asks every provider for its model list, in hours. 0 runs it
    /// only when asked.
    pub discovery_interval_hours: u32,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self {
            unsupported_params: UnsupportedParams::Drop,
            reporting_currency: "USD".into(),
            session_hours: 12,
            remember_me_days: 30,
            require_verified_email: true,
            verification_link_hours: 24,
            reset_link_minutes: 60,
            invitation_link_days: 7,
            audit_rule: None,
            audit_max_body_bytes: 8 << 20,
            breaker: BreakerSettings::default(),
            anthropic_betas_allowed: None,
            provider_tools: None,
            audit: None,
            usage_retention_days: None,
            tag_keys: Vec::new(),
            discovery_interval_hours: 24,
        }
    }
}

impl Doc for GlobalSettings {
    const KIND: &'static str = "settings";
    const PREFIX: &'static str = "settings";
}

/// The id of the single global settings object.
pub const GLOBAL_SETTINGS_ID: &str = "settings_global";

/// Settings an organisation or a workspace may override. `None` inherits.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct ScopeSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unsupported_params: Option<UnsupportedParams>,
    /// The provider-defined tools calls may use, by name without the version: `web_search`,
    /// `web_fetch`, `code_execution`, `bash`, `mcp` (the MCP connector), … Checked on what the
    /// provider receives, in both modes and from every interface. Unset allows all of them; an
    /// empty list allows none. Tools the caller defines itself are not affected. A workspace
    /// that sets this cannot use keys set to blind relay, which cannot be checked.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_tools: Option<Vec<String>>,
    /// Audit mode: recording calls in full.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit: Option<AuditSettings>,
    /// The routing scheme of the organisation that applies to templates which name none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub routing_scheme: Option<String>,
    /// How many days usage events are kept. 0 keeps them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage_retention_days: Option<u32>,
    /// Organisations only: the tag keys of its workspaces that declare none of their own.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag_keys: Option<Vec<String>>,
    /// Organisations only: the names of settings its workspaces may not override.
    /// `routing_scheme` here also keeps templates from naming their own.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub locked: Vec<String>,
}

/// Settings with inheritance applied.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSettings {
    pub unsupported_params: UnsupportedParams,
    /// `None` allows every provider tool.
    pub provider_tools: Option<Vec<String>>,
    /// The workspace's own audit settings (the organisation's when it has none).
    pub audit: AuditSettings,
    /// What the installation or the organisation decided for the workspace, if anything.
    pub audit_rule: Option<AuditRule>,
    pub audit_max_body_bytes: u64,
    /// How many days the workspace's usage events are kept; `None` keeps them.
    pub usage_retention_days: Option<u32>,
    /// The tag keys the workspace's calls may carry: its own, else its organisation's, else
    /// the installation's.
    pub tag_keys: Vec<String>,
}

impl ResolvedSettings {
    pub fn resolve(global: &GlobalSettings, org: &ScopeSettings, ws: &ScopeSettings, ws_tag_keys: &[String]) -> Self {
        let locked = |name: &str| org.locked.iter().any(|l| l == name);
        Self {
            unsupported_params: pick(
                ws.unsupported_params,
                org.unsupported_params,
                global.unsupported_params,
                locked("unsupported_params"),
            ),
            provider_tools: if locked("provider_tools") { None } else { ws.provider_tools.clone() }
                .or_else(|| org.provider_tools.clone())
                .or_else(|| global.provider_tools.clone()),
            audit: if locked("audit") { None } else { ws.audit.clone() }
                .or_else(|| org.audit.clone())
                .or_else(|| global.audit.clone())
                .unwrap_or_default(),
            // The installation's rule comes before the organisation's.
            audit_rule: global.audit_rule.or(org.audit.as_ref().and_then(|a| a.rule)),
            audit_max_body_bytes: global.audit_max_body_bytes,
            usage_retention_days: if locked("usage_retention_days") { None } else { ws.usage_retention_days }
                .or(org.usage_retention_days)
                .or(global.usage_retention_days)
                .filter(|d| *d > 0),
            tag_keys: if !ws_tag_keys.is_empty() && !locked("tag_keys") {
                ws_tag_keys.to_vec()
            } else {
                org.tag_keys.clone().filter(|k| !k.is_empty()).unwrap_or_else(|| global.tag_keys.clone())
            },
        }
    }
}

/// The workspace value unless the organisation locked the setting, then the organisation's,
/// then the global default.
fn pick<T: Copy>(ws: Option<T>, org: Option<T>, global: T, locked: bool) -> T {
    let ws = if locked { None } else { ws };
    ws.or(org).unwrap_or(global)
}

#[cfg(test)]
mod tests {
    use super::*;
    use UnsupportedParams::{Drop, Reject};

    fn s(v: Option<UnsupportedParams>, locked: bool) -> ScopeSettings {
        ScopeSettings {
            unsupported_params: v,
            provider_tools: None,
            audit: None,
            routing_scheme: None,
            usage_retention_days: None,
            tag_keys: None,
            locked: if locked { vec!["unsupported_params".into()] } else { vec![] },
        }
    }

    #[test]
    fn nearest_defined_value_wins() {
        let g = GlobalSettings::default();
        let r =
            |org: ScopeSettings, ws: ScopeSettings| ResolvedSettings::resolve(&g, &org, &ws, &[]).unsupported_params;
        assert_eq!(r(s(None, false), s(None, false)), Drop);
        assert_eq!(r(s(Some(Reject), false), s(None, false)), Reject);
        assert_eq!(r(s(Some(Reject), false), s(Some(Drop), false)), Drop);
        assert_eq!(r(s(None, false), s(Some(Reject), false)), Reject);
    }

    #[test]
    fn an_organisation_lock_overrides_the_workspace() {
        let g = GlobalSettings::default();
        let r =
            |org: ScopeSettings, ws: ScopeSettings| ResolvedSettings::resolve(&g, &org, &ws, &[]).unsupported_params;
        assert_eq!(r(s(Some(Reject), true), s(Some(Drop), false)), Reject);
        assert_eq!(r(s(None, true), s(Some(Reject), false)), Drop);
    }

    #[test]
    fn the_installation_gives_defaults_for_what_nobody_sets() {
        let g = GlobalSettings {
            provider_tools: Some(vec!["web_search".into()]),
            audit: Some(AuditSettings { mode: AuditMode::OnRequest, ..Default::default() }),
            usage_retention_days: Some(90),
            tag_keys: vec!["purpose".into()],
            ..Default::default()
        };
        let none = ScopeSettings::default();
        let r = ResolvedSettings::resolve(&g, &none, &none, &[]);
        assert_eq!(r.provider_tools, Some(vec!["web_search".into()]));
        assert_eq!(r.audit.mode, AuditMode::OnRequest);
        assert_eq!((r.usage_retention_days, r.tag_keys.clone()), (Some(90), vec!["purpose".to_owned()]));

        // The organisation's value comes first, the workspace's before that.
        let org =
            ScopeSettings { usage_retention_days: Some(30), tag_keys: Some(vec!["site".into()]), ..Default::default() };
        let ws = ScopeSettings { usage_retention_days: Some(0), ..Default::default() };
        let r = ResolvedSettings::resolve(&g, &org, &ws, &[]);
        assert_eq!(r.usage_retention_days, None, "0 keeps everything");
        assert_eq!(r.tag_keys, vec!["site".to_owned()]);
        assert_eq!(ResolvedSettings::resolve(&g, &org, &ws, &["team".into()]).tag_keys, vec!["team".to_owned()]);
    }
}
