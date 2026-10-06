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
    /// Rules audit mode out, or makes it mandatory, for the whole installation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit_rule: Option<AuditRule>,
    /// The most one body of an audit record may hold. A larger body is stored up to this
    /// size and marked as truncated.
    pub audit_max_body_bytes: u64,
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
            audit_rule: None,
            audit_max_body_bytes: 8 << 20,
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
    /// The provider-defined tools requests may use on the Anthropic interface, by name
    /// without the version: `web_search`, `code_execution`, `bash`, … Unset allows all of
    /// them; an empty list allows none. Tools the caller defines itself are not affected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_tools: Option<Vec<String>>,
    /// Audit mode: recording calls in full.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audit: Option<AuditSettings>,
    /// Organisations only: the names of settings its workspaces may not override.
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
}

impl ResolvedSettings {
    pub fn resolve(global: &GlobalSettings, org: &ScopeSettings, ws: &ScopeSettings) -> Self {
        let locked = |name: &str| org.locked.iter().any(|l| l == name);
        Self {
            unsupported_params: pick(
                ws.unsupported_params,
                org.unsupported_params,
                global.unsupported_params,
                locked("unsupported_params"),
            ),
            provider_tools: if locked("provider_tools") { None } else { ws.provider_tools.clone() }
                .or_else(|| org.provider_tools.clone()),
            audit: if locked("audit") { None } else { ws.audit.clone() }
                .or_else(|| org.audit.clone())
                .unwrap_or_default(),
            // The installation's rule comes before the organisation's.
            audit_rule: global.audit_rule.or(org.audit.as_ref().and_then(|a| a.rule)),
            audit_max_body_bytes: global.audit_max_body_bytes,
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
            locked: if locked { vec!["unsupported_params".into()] } else { vec![] },
        }
    }

    #[test]
    fn nearest_defined_value_wins() {
        let g = GlobalSettings::default();
        let r = |org: ScopeSettings, ws: ScopeSettings| ResolvedSettings::resolve(&g, &org, &ws).unsupported_params;
        assert_eq!(r(s(None, false), s(None, false)), Drop);
        assert_eq!(r(s(Some(Reject), false), s(None, false)), Reject);
        assert_eq!(r(s(Some(Reject), false), s(Some(Drop), false)), Drop);
        assert_eq!(r(s(None, false), s(Some(Reject), false)), Reject);
    }

    #[test]
    fn an_organisation_lock_overrides_the_workspace() {
        let g = GlobalSettings::default();
        let r = |org: ScopeSettings, ws: ScopeSettings| ResolvedSettings::resolve(&g, &org, &ws).unsupported_params;
        assert_eq!(r(s(Some(Reject), true), s(Some(Drop), false)), Reject);
        assert_eq!(r(s(None, true), s(Some(Reject), false)), Drop);
    }
}
