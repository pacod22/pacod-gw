//! Who calls the admin API: the subject of a token a product's service exchanged
//! for pacod-id (`aud` pacod-id, `act.sub` the service). A person acts with their own bindings, a
//! service account (a script with a `pidt_` or `pgwa_` token, through the product) with its own.
//! The product's service itself gains nothing: a client-credentials token is no actor.
//!
//! The hosted admin pages (`/ui-api/v1/admin/*`) act as the signed-in person, by
//! the session cookie alone; their writes must come from the realm's own pages.

use axum::extract::FromRequestParts;
use axum::http::header;
use axum::http::request::Parts;
use chrono::Utc;
use pacod_id_authz::{Catalog, GrantError, ScopeRef, Subject};
use serde_json::json;

use crate::authz::bindings::subject_bindings;
use crate::error::ApiError;
use crate::http::Shared;
use crate::store::Account;
use crate::store::authz::BindingRow;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActorKind {
    User,
    Service,
}

impl ActorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ActorKind::User => "user",
            ActorKind::Service => "service",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Actor {
    pub id: String,
    pub kind: ActorKind,
    /// The realm whose issuer signed the token.
    pub realm: String,
    /// The product service that exchanged the token, and its product.
    pub client: Option<String>,
    pub product: Option<String>,
    pub subject: Subject,
    pub account: Option<Box<Account>>,
    /// The realm policy (`accounts_managed_by`): the realm whose own sign-ups this actor
    /// manages as if it held `id.accounts.manage`.
    pub manages_signups_of: Option<String>,
}

fn denied() -> ApiError {
    ApiError::unauthorized("send a token exchanged for pacod-id").with_details(json!({ "code": "invalid_token" }))
}

pub fn not_allowed() -> ApiError {
    ApiError::forbidden("you may not do this").with_details(json!({ "code": "not_allowed" }))
}

/// Marks the hosted admin pages' requests: their actor is the session cookie's account.
#[derive(Debug, Clone, Copy)]
pub struct ThroughCookie;

pub async fn through_cookie(mut req: axum::extract::Request, next: axum::middleware::Next) -> axum::response::Response {
    req.extensions_mut().insert(ThroughCookie);
    next.run(req).await
}

impl FromRequestParts<Shared> for Actor {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &Shared) -> Result<Self, ApiError> {
        if parts.extensions.get::<ThroughCookie>().is_some() {
            return Actor::from_cookie(st, parts).await;
        }
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
            .ok_or_else(denied)?
            .trim()
            .to_owned();
        Actor::from_token(st, &token).await
    }
}

impl Actor {
    pub async fn from_token(st: &Shared, token: &str) -> Result<Self, ApiError> {
        let v = st
            .keys
            .verifier()
            .verify(token, &[pacod_id_jose::Alg::RS256, pacod_id_jose::Alg::ES256])
            .map_err(|_| denied())?;
        let c = &v.claims;
        let fresh = c["exp"].as_i64().is_some_and(|e| e > Utc::now().timestamp());
        if v.header.typ.as_deref() != Some("at+jwt") || c["aud"] != "pacod-id" || !fresh {
            return Err(denied());
        }
        let realm = st
            .config
            .realms
            .values()
            .find(|r| c["iss"] == r.issuer.as_str())
            .map(|r| r.name.clone())
            .ok_or_else(denied)?;
        // Exchanged by a product's service: never the service's own token.
        let client = c["act"]["sub"].as_str().and_then(|a| st.config.clients.get(a)).ok_or_else(denied)?;
        let (id, kind, account) = match c["pid_kind"].as_str() {
            Some("user") => {
                let Some(crate::oidc::exchange::Holder::User { account, .. }) =
                    crate::oidc::exchange::user_access_token(st, token, Some("pacod-id"))
                        .await
                        .map_err(|_| denied())?
                else {
                    return Err(denied());
                };
                (account.id.clone(), ActorKind::User, Some(account))
            }
            Some("service") => {
                let id = c["sub"].as_str().ok_or_else(denied)?.to_owned();
                let active: Option<bool> =
                    sqlx::query_scalar("SELECT status = 'active' FROM service_accounts WHERE id = $1")
                        .bind(&id)
                        .fetch_optional(st.store.pool())
                        .await
                        .map_err(ApiError::internal)?;
                if active != Some(true) {
                    return Err(denied());
                }
                (id, ActorKind::Service, None)
            }
            _ => return Err(denied()),
        };
        Actor::assemble(st, id, kind, realm, Some(client.id.clone()), client.product.clone(), account).await
    }

    /// The signed-in person of the hosted pages.
    async fn from_cookie(st: &Shared, parts: &Parts) -> Result<Self, ApiError> {
        let realm =
            crate::oidc::realm_by_host(st, &parts.headers).ok_or_else(|| ApiError::not_found("no realm here"))?;
        if !matches!(parts.method, axum::http::Method::GET | axum::http::Method::HEAD) {
            crate::oidc::flows::same_origin(realm, &parts.headers)?;
        }
        let (_, account) = crate::oidc::browser_session(st, realm, &parts.headers).await?.ok_or_else(|| {
            ApiError::unauthorized("sign in first").with_details(json!({ "code": "sign_in_required" }))
        })?;
        Actor::assemble(
            st,
            account.id.clone(),
            ActorKind::User,
            realm.name.clone(),
            None,
            None,
            Some(Box::new(account)),
        )
        .await
    }

    async fn assemble(
        st: &Shared,
        id: String,
        kind: ActorKind,
        realm: String,
        client: Option<String>,
        product: Option<String>,
        account: Option<Box<Account>>,
    ) -> Result<Self, ApiError> {
        let mut conn = st.store.pool().acquire().await.map_err(ApiError::internal)?;
        let held = subject_bindings(&mut conn, &id).await?;
        let subject = if kind == ActorKind::User { Subject::user(held) } else { Subject::service(held) };
        let catalog = st.catalog.get();
        let policy = &st.config.realms[&realm].accounts_managed_by;
        let manages_signups_of =
            policy.iter().any(|role| catalog.has_role(&subject, role, &ScopeRef::platform())).then(|| realm.clone());
        Ok(Actor { id, kind, realm, client, product, subject, account, manages_signups_of })
    }

    pub fn allowed(&self, catalog: &Catalog, permission: &str, at: &[ScopeRef]) -> bool {
        catalog.allowed(&self.subject, permission, at)
    }

    fn platform(&self, catalog: &Catalog, permission: &str) -> bool {
        self.allowed(catalog, permission, &[ScopeRef::platform()])
    }

    /// Whether the actor sees accounts: an identity admin or auditor, or the realm policy.
    pub fn reads_accounts(&self, catalog: &Catalog) -> bool {
        self.platform(catalog, "id.accounts.read") || self.manages_signups_of.is_some()
    }

    /// Whether the actor sees all accounts, not only one realm's sign-ups.
    pub fn reads_all_accounts(&self, catalog: &Catalog) -> bool {
        self.platform(catalog, "id.accounts.read")
    }

    /// Whether the actor manages this account: `id.accounts.manage`, or the realm policy for an
    /// account that signed up in that realm.
    pub fn manages_account(&self, catalog: &Catalog, account: &Account) -> bool {
        self.platform(catalog, "id.accounts.manage")
            || self.manages_signups_of.as_deref() == Some(account.signup_realm.as_str())
    }

    /// Whether the actor may create accounts in its realm.
    pub fn creates_accounts(&self, catalog: &Catalog) -> bool {
        self.platform(catalog, "id.accounts.manage") || self.manages_signups_of.is_some()
    }

    /// The permission that makes someone an administrator of the acting product: its manifest's
    /// grant default (the gateway's `gw.members.manage`).
    fn product_admin_permission(&self, catalog: &Catalog) -> Option<String> {
        let product = self.product.as_deref()?;
        Some(catalog.manifest(product)?.grant_default().to_owned())
    }

    /// Whether the actor administers what is at `path` for its product, or is an identity admin.
    pub fn administers(&self, catalog: &Catalog, path: &[ScopeRef]) -> bool {
        self.platform(catalog, "id.bindings.manage")
            || self.product_admin_permission(catalog).is_some_and(|p| self.allowed(catalog, &p, path))
    }

    /// Service accounts at `path` (the platform, or an organisation).
    pub fn manages_service_accounts(&self, catalog: &Catalog, path: &[ScopeRef]) -> bool {
        self.allowed(catalog, "id.service_accounts.manage", path) || self.administers(catalog, path)
    }

    /// Whether the actor may give or take `role` at `path`.
    pub fn may_grant(&self, catalog: &Catalog, role: &str, path: &[ScopeRef]) -> Result<(), ApiError> {
        match catalog.can_grant(&self.subject, role, path) {
            Ok(()) => Ok(()),
            Err(GrantError::NotPermitted) if self.platform(catalog, "id.bindings.manage") => Ok(()),
            Err(GrantError::NotPermitted) => Err(not_allowed()),
            Err(e) => Err(ApiError::invalid(e.to_string()).with_details(json!({ "code": e.code() }))),
        }
    }

    /// Whether the actor may see a scope's members: an identity admin or auditor, or someone
    /// holding a role of a product that has the scope at it or above it.
    pub fn sees_members(&self, catalog: &Catalog, path: &[ScopeRef], products: &[String]) -> bool {
        if self.platform(catalog, "id.accounts.read") || self.administers(catalog, path) {
            return true;
        }
        self.subject.bindings.iter().any(|b| {
            let product = b.role.split_once('.').map_or("", |(p, _)| p);
            products.iter().any(|p| p == product)
                && b.path.len() <= path.len()
                && path.starts_with(&b.path)
                && catalog.manifest(product).is_some_and(|m| m.role(&b.role).is_some())
        })
    }

    /// The actor's own binding?
    pub fn owns(&self, b: &BindingRow) -> bool {
        b.principal_id == self.id
    }

    /// The actor as audit entries name it.
    pub fn audit(&self, action: &str) -> crate::store::AuditEntry {
        crate::store::AuditEntry {
            realm: Some(self.realm.clone()),
            client_id: self.client.clone(),
            ..crate::store::AuditEntry::new(action).by(&self.id, self.kind.as_str())
        }
    }
}
