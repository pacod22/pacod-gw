//! The management API (concept section 10): everything the UI and automation do goes through
//! here. It is served on its own listener.

pub mod account;
pub mod auth;
pub mod crud;
pub mod discovery;
pub mod error;
pub mod facade;
pub mod members;
pub mod monitoring;
pub mod openapi;
pub mod playground;
pub mod resources;
pub mod session;
pub mod system;
pub mod transfer;
pub mod upkeep;

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::Value;

use crate::bootstrap::Bootstrap;
use crate::limits::LimitEngine;
use crate::model::{Doc, RoleBinding, RoleName, RoleScope, Status, User, new_id};
use crate::snapshot::ConfigState;
use crate::store::{AuditEntry, Store, Tx};
use crate::telemetry::live::LiveStats;
use crate::upstream::Upstream;
use crate::upstream::egress::EgressPolicy;
use error::{ApiError, ApiResult};

#[derive(Clone)]
pub struct AdminState {
    pub boot: Arc<Bootstrap>,
    pub config: Arc<ConfigState>,
    pub store: Store,
    pub egress: Arc<EgressPolicy>,
    /// Client for calls to the analytics service.
    pub http: reqwest::Client,
    pub logins: Arc<session::LoginGuard>,
    /// Slows down admin-token guessing per client address.
    pub token_failures: Arc<crate::dataplane::auth::FailureLimiter>,
    /// Sends account emails. `None` when the installation has no SMTP server configured.
    pub mailer: Option<Arc<crate::mail::Mailer>>,
    /// Limits how often one address may ask for an account email.
    pub account_requests: Arc<account::RequestGuard>,
    /// Live counters of the data plane in this process. Empty when it only serves the
    /// management API.
    pub live: Arc<LiveStats>,
    /// The limit counters, shared with the data plane of this process.
    pub limits: Arc<LimitEngine>,
    /// The client used for provider probes; it applies the egress policy.
    pub upstream: Upstream,
    /// The data plane of this process: the playground makes its test calls through it.
    pub gw: Arc<crate::dataplane::Gw>,
    pub openapi: Arc<Value>,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

impl AdminState {
    /// Commits a configuration change and applies it on this instance before returning, so
    /// that the caller's next request already sees it.
    pub async fn commit(&self, tx: Tx) -> ApiResult<Option<i64>> {
        let version = tx.commit().await?;
        if let Some(v) = version {
            self.config.ensure_at_least(v).await?;
        }
        Ok(version)
    }

    pub fn analytics_url(&self) -> ApiResult<&str> {
        self.boot
            .analytics_url
            .as_deref()
            .ok_or_else(|| ApiError::unavailable("the analytics service is not configured in this installation"))
    }
}

/// Builds the routes of the management API and its OpenAPI document. The document depends
/// only on the code, so it can be produced without a database.
pub fn build() -> (Router<AdminState>, Value) {
    let mut api = openapi::OpenApi::new();
    let mut v1: Router<AdminState> = Router::new();
    v1 = session::routes(v1, &mut api);
    v1 = account::routes(v1, &mut api);
    v1 = resources::tenancy::routes(v1, &mut api);
    v1 = resources::provider::routes(v1, &mut api);
    v1 = resources::access::routes(v1, &mut api);
    v1 = members::routes(v1, &mut api);
    v1 = resources::repo::routes(v1, &mut api);
    v1 = resources::template::routes(v1, &mut api);
    v1 = resources::limit::routes(v1, &mut api);
    v1 = resources::pricejob::routes(v1, &mut api);
    v1 = system::routes(v1, &mut api);
    v1 = playground::routes(v1, &mut api);
    v1 = monitoring::routes(v1, &mut api);
    v1 = transfer::routes(v1, &mut api);
    v1 = facade::routes(v1, &mut api);
    // Last, so that the numbered schemas of the resources before it keep their names.
    v1 = resources::routing::routes(v1, &mut api);
    v1 = resources::rateclass::routes(v1, &mut api);
    v1 = discovery::routes(v1, &mut api);
    let doc = api.finish(env!("CARGO_PKG_VERSION"));
    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .nest("/api/v1", v1.route("/openapi.json", get(openapi_doc)));
    (router, doc)
}

/// The OpenAPI document alone, for the `openapi` command and the drift check.
pub fn openapi_document() -> Value {
    build().1
}

async fn openapi_doc(State(st): State<AdminState>) -> Json<Value> {
    Json((*st.openapi).clone())
}

async fn healthz() -> Json<Value> {
    Json(serde_json::json!({ "status": "ok" }))
}

async fn readyz(State(st): State<AdminState>) -> (StatusCode, Json<Value>) {
    if st.store.ping().await {
        (StatusCode::OK, Json(serde_json::json!({ "status": "ready", "config_version": st.config.version() })))
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, Json(serde_json::json!({ "status": "database unreachable" })))
    }
}

/// Creates the first platform admin from the bootstrap values when no user exists yet.
/// Creates the built-in `_system` organisation and workspace when they are missing, and moves
/// the price job's extraction template there once (concept sections 5.1 and 13.3). Several
/// instances may start at once: whoever comes second finds them there.
pub async fn ensure_system(store: &Store) -> anyhow::Result<()> {
    use crate::model::{
        Organisation, PRICE_JOB_ID, PriceJob, SYSTEM_NAME, SYSTEM_ORG_ID, SYSTEM_WS_ID, Template, Workspace,
    };
    let system = || AuditEntry { actor: "bootstrap".into(), actor_kind: "system".into(), ..Default::default() };
    let mut tx = store.begin().await?;
    if tx.get(SYSTEM_ORG_ID).await?.is_none() {
        let org = Organisation {
            name: SYSTEM_NAME.into(),
            display_name: "The gateway's own calls".into(),
            status: Status::Active,
            settings: Default::default(),
        };
        let audit = AuditEntry { organisation_id: Some(SYSTEM_ORG_ID.into()), ..system() };
        tx.create(Organisation::KIND, SYSTEM_ORG_ID, None, Some(SYSTEM_NAME), &serde_json::to_value(&org)?, audit)
            .await?;
    }
    if tx.get(SYSTEM_WS_ID).await?.is_none() {
        let ws: Workspace = serde_json::from_value(serde_json::json!({
            "name": SYSTEM_NAME, "display_name": "The gateway's own calls", "tag_keys": ["purpose"]
        }))?;
        let audit = AuditEntry {
            organisation_id: Some(SYSTEM_ORG_ID.into()),
            workspace_id: Some(SYSTEM_WS_ID.into()),
            ..system()
        };
        tx.create(
            Workspace::KIND,
            SYSTEM_WS_ID,
            Some(SYSTEM_ORG_ID),
            Some(SYSTEM_NAME),
            &serde_json::to_value(&ws)?,
            audit,
        )
        .await?;
    }
    // A price job that names an extraction template elsewhere: the template is copied into
    // `_system` and the job points there from now on.
    if let Some(row) = tx.get(PRICE_JOB_ID).await? {
        let mut job: PriceJob = serde_json::from_value(row.doc.clone())?;
        if let Some(x) = job.extraction.as_mut().filter(|x| x.workspace_id != SYSTEM_WS_ID) {
            let in_system = tx
                .list(Template::KIND, Some(SYSTEM_WS_ID))
                .await?
                .into_iter()
                .any(|r| r.name.as_deref() == Some(&x.template));
            if !in_system
                && let Some(source) = tx
                    .list(Template::KIND, Some(&x.workspace_id))
                    .await?
                    .into_iter()
                    .find(|r| r.name.as_deref() == Some(&x.template))
            {
                let audit = AuditEntry {
                    organisation_id: Some(SYSTEM_ORG_ID.into()),
                    workspace_id: Some(SYSTEM_WS_ID.into()),
                    ..system()
                };
                tx.create(
                    Template::KIND,
                    &new_id(Template::PREFIX),
                    Some(SYSTEM_WS_ID),
                    Some(&x.template),
                    &source.doc,
                    audit,
                )
                .await?;
            }
            tracing::info!(from = %x.workspace_id, template = %x.template, "the price job's extraction template now lives in _system");
            x.workspace_id = SYSTEM_WS_ID.into();
            tx.update(PRICE_JOB_ID, None, None, &serde_json::to_value(&job)?, system()).await?;
        }
    }
    match tx.commit().await {
        Ok(_) => Ok(()),
        // Another instance created them at the same moment.
        Err(e) if e.to_string().contains("already exists") || e.to_string().contains("duplicate key") => Ok(()),
        Err(e) => Err(e.into()),
    }
}

pub async fn ensure_first_admin(store: &Store, boot: &Bootstrap) -> anyhow::Result<bool> {
    let mut tx = store.begin().await?;
    if tx.count_kind(User::KIND).await? > 0 {
        return Ok(false);
    }
    let (Some(email), Some(password)) = (&boot.admin_email, &boot.admin_password) else {
        tracing::warn!(
            "no user exists and PGW_ADMIN_EMAIL / PGW_ADMIN_PASSWORD are not set: nobody can log in to the management API"
        );
        return Ok(false);
    };
    anyhow::ensure!(
        password.chars().count() >= resources::access::MIN_PASSWORD_LEN,
        "PGW_ADMIN_PASSWORD must be at least {} characters",
        resources::access::MIN_PASSWORD_LEN
    );
    let system = || AuditEntry { actor: "bootstrap".into(), actor_kind: "system".into(), ..Default::default() };
    let user_id = new_id(User::PREFIX);
    let user = User {
        email: email.trim().to_lowercase(),
        display_name: "Platform admin".into(),
        password_hash: crate::crypto::password::hash(password)?,
        status: Status::Active,
        // Whoever sets PGW_ADMIN_EMAIL controls the installation; there is nothing to confirm.
        email_verified: true,
    };
    tx.create(User::KIND, &user_id, None, Some(&user.email), &serde_json::to_value(&user)?, system()).await?;
    let binding = RoleBinding { principal: user_id.clone(), role: RoleName::PlatformAdmin, scope: RoleScope::Platform };
    tx.create(
        RoleBinding::KIND,
        &new_id(RoleBinding::PREFIX),
        None,
        Some(&format!("{user_id}|PlatformAdmin|platform")),
        &serde_json::to_value(&binding)?,
        system(),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(email = %user.email, "created the first platform admin");
    Ok(true)
}
