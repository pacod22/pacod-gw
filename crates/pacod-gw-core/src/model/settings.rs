//! Settings and how they are inherited (concept section 5.3): the nearest defined value wins
//! (workspace, then organisation, then global), unless the organisation locked it.

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

/// Installation-wide defaults. A single object with the id `settings_global`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(default)]
pub struct GlobalSettings {
    pub unsupported_params: UnsupportedParams,
    /// The currency reports are shown in.
    pub reporting_currency: String,
    /// Session lifetime of console logins, in hours.
    pub session_hours: u32,
}

impl Default for GlobalSettings {
    fn default() -> Self {
        Self { unsupported_params: UnsupportedParams::Drop, reporting_currency: "USD".into(), session_hours: 12 }
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
