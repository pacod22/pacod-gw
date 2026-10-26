//! The configuration objects of the gateway.
//!
//! Each type here is one kind of document in the `config_objects` table. The same types are
//! what the snapshot is built from and what the management API reads and writes, so a field
//! added here is stored, hot-reloaded and exposed without further plumbing.

pub mod access;
pub mod limit;
pub mod plugin;
pub mod pricejob;
pub mod provider;
pub mod rateclass;
pub mod repo;
pub mod routing;
pub mod settings;
pub mod template;
pub mod tenancy;

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

pub use access::{AdminToken, RoleBinding, RoleName, RoleScope, User};
pub use limit::{
    EndUserOverride, Enforcement, LimitFilter, LimitSpec, LimitSubject, Metric, OrgLimit, Window, WorkspaceLimit,
};
pub use plugin::{
    Catalogue, OnError, OrgIntentCatalogue, OrgPluginBinding, PluginBinding, PluginInstall, PluginMode,
    ResponseCacheClear, WorkspaceIntentCatalogue, WorkspacePluginBinding,
};
pub use pricejob::{Extraction, FeedFormat, PRICE_JOB_ID, PriceJob, PriceSourceSpec, SourceKind};
pub use provider::{
    AuthScheme, Credential, CredentialScope, CredentialState, EchoReasoning, ErrorRules, Family, ManagedApi, Platform,
    Provider, ProviderPreset, Quirks, ReasoningParam, RerankApi, VertexSite, provider_presets,
};
pub use rateclass::RateClass;
pub use repo::{
    Capabilities, MaxTokensField, Modalities, Model, ModelKind, ModelState, OrgGrant, ParamSupport, Price, PriceSource,
    PriceStatus, PriceTier, PriceWindow, PromptCacheStyle, Rates, ReasoningStyle, ThinkingOff, WorkspaceGrant,
};
pub use routing::{
    Affinity, Algorithm, Complexity, ComplexityMatch, FailoverOn, FailoverPolicy, Overflow, RouteAction, RouteMatch,
    RouteRule, RouteSource, RoutingScheme, StrMatch, TokenRange, default_routing_schemes,
};
pub use settings::{
    AuditAttachments, AuditGuarantee, AuditMode, AuditRule, AuditSettings, BreakerSettings, GlobalSettings,
    HolidayCalendar, HolidayDates, ScopeSettings, UnsupportedParams,
};
pub use template::{
    AuditSwitch, Param, PoolEntry, PromptCacheMode, ProxyBehaviour, ReasoningSpec, Rule, Template, TemplateSpec,
    TemplateVersion,
};
pub use tenancy::{
    ApiKey, InspectMode, KeyStatus, Mode, OrgConfigTemplate, Organisation, SYSTEM_NAME, SYSTEM_ORG_ID, SYSTEM_WS_ID,
    UsagePolicy, Workspace, all_modes, org_config_template,
};

/// A kind of configuration object.
pub trait Doc: Serialize + DeserializeOwned + Clone + Send + Sync + 'static {
    /// The value of the `kind` column, for example `provider`.
    const KIND: &'static str;
    /// The prefix of ids of this kind, for example `prv`.
    const PREFIX: &'static str;
}

/// Generates an id such as `ws_018f3c2a…`. Ids sort by creation time.
pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid::Uuid::now_v7().simple())
}

/// A stored object: its document plus the columns every object has.
#[derive(Debug, Clone, PartialEq)]
pub struct Obj<T> {
    pub id: String,
    pub parent: Option<String>,
    pub name: Option<String>,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub doc: T,
}

/// The columns every object has, as returned by the management API next to the document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Meta {
    pub id: String,
    /// Incremented on every update. Send it back on update to detect concurrent changes.
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl<T> Obj<T> {
    pub fn meta(&self) -> Meta {
        Meta { id: self.id.clone(), version: self.version, created_at: self.created_at, updated_at: self.updated_at }
    }
}

/// Lower-case letters, digits and hyphens, starting with a letter or digit, at most 63 long.
/// Used for the names that appear in URLs and model strings.
pub fn is_slug(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 63
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !s.starts_with('-')
        && !s.ends_with('-')
}

/// Active or switched off. Shared by the kinds that have no richer life cycle.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    #[default]
    Active,
    Disabled,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        for ok in ["a", "acme-prod", "ws1", "0x"] {
            assert!(is_slug(ok), "{ok}");
        }
        for bad in ["", "-a", "a-", "A", "a b", "a_b", "é", &"a".repeat(64)] {
            assert!(!is_slug(bad), "{bad}");
        }
    }

    #[test]
    fn ids_carry_the_prefix_and_sort_by_time() {
        let a = new_id("ws");
        let b = new_id("ws");
        assert!(a.starts_with("ws_") && a.len() == 3 + 32);
        assert!(a < b);
    }
}
