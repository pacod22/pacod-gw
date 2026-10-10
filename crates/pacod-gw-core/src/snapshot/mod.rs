//! The in-memory configuration the request path reads (concept section 11.2).
//!
//! A snapshot is immutable. A configuration change builds a new one and swaps it in atomically;
//! requests already running keep the one they started with.

mod build;
mod reload;

use std::collections::HashMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use zeroize::Zeroizing;

use crate::model::settings::ResolvedSettings;
use crate::model::{
    AdminToken, ApiKey, EndUserOverride, GlobalSettings, LimitSpec, Model, Obj, OrgGrant, Organisation, Price,
    Provider, RoleBinding, TemplateSpec, User, Workspace, WorkspaceGrant,
};
use crate::routing::{BoundRoute, CompiledScheme, PoolMember};

pub use build::{build, build_with};
pub use reload::{ConfigState, InstanceInfo};

/// A decrypted provider credential.
pub struct Secret {
    /// The credential object's id.
    pub id: String,
    pub value: Zeroizing<String>,
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secret").field("id", &self.id).finish_non_exhaustive()
    }
}

/// Whose credential served a call. Provider rate-limit headers are passed on only for `Own`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CredentialOwner {
    /// The installation's shared credential.
    Shared,
    /// A credential of the caller's own organisation or workspace.
    Own,
}

#[derive(Debug, Default)]
pub struct Credentials {
    pub global: Option<Arc<Secret>>,
    pub organisations: HashMap<String, Arc<Secret>>,
    pub workspaces: HashMap<String, Arc<Secret>>,
}

#[derive(Debug)]
pub struct ProviderRt {
    pub id: String,
    pub doc: Provider,
    /// Where proxy mode sends the relayed path. Resolved at build time.
    pub proxy_origin: String,
    pub credentials: Credentials,
    /// Where this provider reports cached and reasoning tokens, from its quirk profile.
    pub usage_fields: Arc<crate::extract::UsageFields>,
}

impl ProviderRt {
    /// The credential for a workspace: its own, then its organisation's, then the shared one.
    pub fn credential(&self, workspace_id: &str, organisation_id: &str) -> Option<(Arc<Secret>, CredentialOwner)> {
        if let Some(s) = self.credentials.workspaces.get(workspace_id) {
            return Some((s.clone(), CredentialOwner::Own));
        }
        if let Some(s) = self.credentials.organisations.get(organisation_id) {
            return Some((s.clone(), CredentialOwner::Own));
        }
        self.credentials.global.clone().map(|s| (s, CredentialOwner::Shared))
    }
}

#[derive(Debug)]
pub struct ModelRt {
    pub id: String,
    pub doc: Model,
}

/// A limit with the scope it belongs to.
#[derive(Debug)]
pub struct LimitRt {
    pub id: String,
    pub spec: LimitSpec,
    /// The organisation or workspace that owns the limit.
    pub owner_id: String,
}

/// The current version of a template, ready to use.
#[derive(Debug)]
pub struct TemplateRt {
    pub id: String,
    pub name: String,
    pub version: u32,
    pub spec: TemplateSpec,
    /// The object id of the model listed for the template: its single model, or the first of
    /// its pool by priority.
    pub model_id: String,
    /// The models of the template whose model exists. Never empty.
    pub pool: Box<[PoolMember]>,
    /// How a call chooses among them.
    pub route: BoundRoute,
}

#[derive(Debug)]
pub struct OrgRt {
    pub id: String,
    pub doc: Organisation,
    /// Models the organisation may use, by model object id.
    pub grants: HashMap<String, OrgGrant>,
    pub limits: Vec<Arc<LimitRt>>,
    /// Routing schemes by name.
    pub schemes: HashMap<String, Arc<CompiledScheme>>,
}

/// What a workspace may do with a model it is allowed to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GrantTerms {
    /// Serve requests for a retired model with its successor.
    pub follow_successor: bool,
    /// The lowest output-token ceiling the grants set, if any.
    pub max_output_tokens: Option<u32>,
}

#[derive(Debug)]
pub struct WorkspaceRt {
    pub id: String,
    pub organisation_id: String,
    pub doc: Workspace,
    /// Settings with inheritance applied.
    pub settings: ResolvedSettings,
    /// False when the workspace or its organisation is disabled.
    pub enabled: bool,
    /// The workspace's own grants by model object id. `None` when it has none, which means it
    /// may use every model of its organisation.
    pub grants: Option<HashMap<String, WorkspaceGrant>>,
    /// Templates by name.
    pub templates: HashMap<String, Arc<TemplateRt>>,
    /// The workspace's own limits followed by its organisation's.
    pub limits: Vec<Arc<LimitRt>>,
    /// End-user overrides by (limit id, end user).
    pub overrides: HashMap<(String, String), EndUserOverride>,
}

#[derive(Debug)]
pub struct KeyRt {
    pub id: String,
    pub workspace_id: String,
    pub organisation_id: String,
    pub doc: ApiKey,
    /// For an entry reached through the previous secret after a rotation: when it stops working.
    pub valid_until: Option<DateTime<Utc>>,
}

impl KeyRt {
    /// What proxy mode does when a response carries no usage: the key's own policy, or, when it
    /// has none, `require` while an enforced token or spend limit applies to its workspace
    /// (concept section 4.2) and `estimate` otherwise.
    pub fn usage_policy(&self, ws: &WorkspaceRt) -> crate::model::UsagePolicy {
        use crate::model::{Enforcement, Metric, UsagePolicy};
        self.doc.usage_policy.unwrap_or_else(|| {
            let counts_usage = ws.limits.iter().any(|l| {
                l.spec.enforcement == Enforcement::Request
                    && matches!(
                        l.spec.metric,
                        Metric::Tokens | Metric::InputTokens | Metric::OutputTokens | Metric::Spend
                    )
            });
            if counts_usage { UsagePolicy::Require } else { UsagePolicy::Estimate }
        })
    }
}

#[derive(Debug, Default)]
pub struct Snapshot {
    /// The configuration version this snapshot was built from.
    pub version: i64,
    pub settings: GlobalSettings,
    /// The price tracker's configuration.
    pub price_job: crate::model::PriceJob,
    pub organisations: HashMap<String, Arc<OrgRt>>,
    pub workspaces: HashMap<String, Arc<WorkspaceRt>>,
    /// API keys by the HMAC of their secret.
    pub keys: HashMap<[u8; 32], Arc<KeyRt>>,
    pub providers: HashMap<String, Arc<ProviderRt>>,
    /// Provider slug to provider id.
    pub provider_names: HashMap<String, String>,
    /// Models by object id.
    pub models: HashMap<String, Arc<ModelRt>>,
    /// Model id (the name applications use) to object id.
    pub model_names: HashMap<String, String>,
    /// Active price versions by model object id, newest first.
    pub prices: HashMap<String, Vec<Arc<Price>>>,
    pub users: HashMap<String, Arc<Obj<User>>>,
    /// Login name (lower case) to user id.
    pub user_emails: HashMap<String, String>,
    /// Admin tokens by the HMAC of their secret.
    pub admin_tokens: HashMap<[u8; 32], Arc<Obj<AdminToken>>>,
    pub role_bindings: Vec<Obj<RoleBinding>>,
    /// Objects that could not be used, for example a credential that does not decrypt.
    /// Shown on the system status page.
    pub problems: Vec<String>,
}

impl Snapshot {
    pub fn provider_by_name(&self, name: &str) -> Option<&Arc<ProviderRt>> {
        self.provider_names.get(name).and_then(|id| self.providers.get(id))
    }

    pub fn model_by_name(&self, name: &str) -> Option<&Arc<ModelRt>> {
        self.model_names.get(name).and_then(|id| self.models.get(id))
    }

    /// The repository model at a provider that a provider's own model id names, if any.
    pub fn model_at_provider(&self, provider_id: &str, upstream: &str) -> Option<&Arc<ModelRt>> {
        let at = || self.models.values().filter(|m| m.doc.provider_id == provider_id);
        at().find(|m| m.doc.upstream_id == upstream)
            .or_else(|| at().find(|m| crate::relay::recorder::same_upstream_model(upstream, &m.doc.upstream_id)))
    }

    /// Looks up an API key by the HMAC of the presented secret. Honours expiry, revocation and
    /// the grace period of a rotated secret.
    pub fn key(&self, hmac: &[u8; 32], now: DateTime<Utc>) -> Option<&Arc<KeyRt>> {
        let k = self.keys.get(hmac)?;
        (k.doc.usable(now) && k.valid_until.is_none_or(|t| now < t)).then_some(k)
    }

    pub fn workspace_of(&self, key: &KeyRt) -> Option<&Arc<WorkspaceRt>> {
        self.workspaces.get(&key.workspace_id).filter(|w| w.enabled)
    }

    /// Whether a workspace may use a model, and on what terms (concept section 5.3: allow-lists
    /// intersect, ceilings take the minimum).
    pub fn grant_terms(&self, ws: &WorkspaceRt, model_id: &str) -> Option<GrantTerms> {
        let org = self.organisations.get(&ws.organisation_id)?.grants.get(model_id)?;
        let mut terms = GrantTerms { follow_successor: org.follow_successor, max_output_tokens: org.max_output_tokens };
        if let Some(own) = &ws.grants {
            let g = own.get(model_id)?;
            terms.follow_successor &= g.follow_successor;
            terms.max_output_tokens = match (terms.max_output_tokens, g.max_output_tokens) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
        }
        Some(terms)
    }

    /// The price that applies to a model at `at`: the newest active version already in effect.
    pub fn price_at(&self, model_id: &str, at: DateTime<Utc>) -> Option<&Arc<Price>> {
        self.prices.get(model_id)?.iter().find(|p| p.effective_from <= at)
    }
}
