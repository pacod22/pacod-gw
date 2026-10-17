//! The management API (concept section 10): everything the UI and automation do goes through
//! here. It is served on its own listener.

pub mod account;
pub mod auth;
pub mod crud;
pub mod discovery;
pub mod error;
pub mod facade;
pub mod idfacade;
pub mod members;
pub mod monitoring;
pub mod objects;
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
    /// Job progress and health changes for the live feed (`GET /events`), besides the
    /// configuration changes.
    pub feed: tokio::sync::broadcast::Sender<FeedEvent>,
    /// pacod-id, when it signs people in (`PGW_AUTH=oidc`); `None` in local mode.
    pub identity: Option<Arc<crate::identity::Identity>>,
}

/// An event of the live feed other than a configuration change: platform roles only.
#[derive(Debug, Clone, serde::Serialize)]
pub struct FeedEvent {
    /// `job` or `health`.
    #[serde(skip)]
    pub event: &'static str,
    #[serde(flatten)]
    pub data: Value,
}

tokio::task_local! {
    /// The configuration version the request being answered produced, if it changed anything.
    static COMMITTED: std::cell::Cell<Option<i64>>;
    /// The position in pacod-id's feed an identity change of the request produced (a facade).
    static IDENTITY_SEQ: std::cell::Cell<Option<i64>>;
}

impl AdminState {
    /// Commits a configuration change and applies it on this instance before returning, so
    /// that the caller's next request already sees it.
    pub async fn commit(&self, tx: Tx) -> ApiResult<Option<i64>> {
        let version = tx.commit().await?;
        if let Some(v) = version {
            self.config.ensure_at_least(v).await?;
            let _ = COMMITTED.try_with(|c| c.set(Some(c.get().map_or(v, |old| old.max(v)))));
        }
        Ok(version)
    }

    /// After an identity change through a facade: waits (up to 2 seconds) until this
    /// instance's copy has pacod-id's answer `seq`, so that the caller's next request sees the
    /// change, and notes it for the answer's `x-pid-seq` and `x-gw-applied` (concept 7.4).
    pub async fn identity_written(&self, seq: Option<i64>) {
        let (Some(seq), Some(id)) = (seq, &self.identity) else { return };
        if !id.cache.wait_for(seq, std::time::Duration::from_secs(2)).await {
            tracing::warn!(seq, "the copy of pacod-id's roles did not reach a change within 2 seconds");
        }
        let _ = IDENTITY_SEQ.try_with(|c| c.set(Some(c.get().map_or(seq, |old| old.max(seq)))));
    }

    /// Of the management instances following pacod-id, how many have `seq`, of how many. This
    /// instance counts as applied: it waited for the change before answering.
    pub async fn identity_applied(&self, seq: i64) -> (i64, i64) {
        match self.store.instances_authz_applied(crate::cluster::ALIVE_SECONDS, seq).await {
            Ok((applied, of)) if of > 0 => {
                let here_behind = self.identity.as_ref().is_some_and(|id| id.cache.seq() >= seq);
                (applied.max(i64::from(here_behind)), of)
            }
            _ => (1, 1),
        }
    }

    /// How many running instances have applied `version`, of how many (concept section 11.2).
    /// This instance counts as applied: a change is applied here before it is answered.
    pub async fn applied(&self, version: i64) -> (usize, usize) {
        match self.store.instances(crate::cluster::ALIVE_SECONDS).await {
            Ok(list) if !list.is_empty() => {
                let here = list.iter().any(|i| i.id == self.boot.instance_id);
                let applied =
                    list.iter().filter(|i| i.applied_version >= version || i.id == self.boot.instance_id).count();
                let of = list.len() + usize::from(!here);
                (applied + usize::from(!here), of)
            }
            _ => (1, 1),
        }
    }

    /// Tells the live feed's listeners about job progress or a health change.
    pub fn announce(&self, event: &'static str, data: Value) {
        let _ = self.feed.send(FeedEvent { event, data });
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
/// Answers that changed the configuration say so (concept section 10.1): `x-gw-config-version`
/// is the version the change produced, `x-gw-applied` how many running instances have applied
/// it, as `n/m`.
pub async fn applied_headers(
    State(st): State<AdminState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let (mut response, version, identity_seq) = COMMITTED
        .scope(
            std::cell::Cell::new(None),
            IDENTITY_SEQ.scope(std::cell::Cell::new(None), async move {
                let response = next.run(req).await;
                (response, COMMITTED.with(std::cell::Cell::get), IDENTITY_SEQ.with(std::cell::Cell::get))
            }),
        )
        .await;
    // An identity change (concept 7.4): x-pid-seq instead of the configuration version.
    if let Some(seq) = identity_seq {
        let (applied, of) = st.identity_applied(seq).await;
        let h = response.headers_mut();
        if let Ok(value) = axum::http::HeaderValue::from_str(&seq.to_string()) {
            h.insert("x-pid-seq", value);
        }
        if let Ok(value) = axum::http::HeaderValue::from_str(&format!("{applied}/{of}")) {
            h.insert("x-gw-applied", value);
        }
    }
    if let Some(v) = version {
        let (applied, of) = st.applied(v).await;
        let h = response.headers_mut();
        if let Ok(value) = axum::http::HeaderValue::from_str(&v.to_string()) {
            h.insert("x-gw-config-version", value);
        }
        if let Ok(value) = axum::http::HeaderValue::from_str(&format!("{applied}/{of}")) {
            h.insert("x-gw-applied", value);
        }
    }
    response
}

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
    v1 = objects::routes(v1, &mut api);
    v1 = resources::plugin::routes(v1, &mut api);
    v1 = resources::intent_try::routes(v1, &mut api);
    let doc = api.finish(env!("CARGO_PKG_VERSION"));
    let dry_runs = Arc::new(dry_run_operations(&doc));
    let v1 = v1.route_layer(axum::middleware::from_fn(
        move |path: axum::extract::MatchedPath, req: axum::extract::Request, next: axum::middleware::Next| {
            let dry_runs = dry_runs.clone();
            async move { refuse_unknown_dry_run(&dry_runs, path, req, next).await }
        },
    ));
    let router = Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .nest("/api/v1", v1.route("/openapi.json", get(openapi_doc)));
    (router, doc)
}

/// The operations with a dry run, as `(method, path)`: those that declare `dry_run` in the
/// OpenAPI document.
fn dry_run_operations(doc: &Value) -> std::collections::HashSet<(String, String)> {
    let mut out = std::collections::HashSet::new();
    for (path, ops) in doc["paths"].as_object().into_iter().flatten() {
        for (method, op) in ops.as_object().into_iter().flatten() {
            if op["parameters"].as_array().is_some_and(|ps| ps.iter().any(|p| p["name"] == "dry_run")) {
                out.insert((method.clone(), path.clone()));
            }
        }
    }
    out
}

/// A change asked for as a dry run where there is none is refused, not made: an ignored
/// `?dry_run=true` would carry out what the caller only wanted to see.
async fn refuse_unknown_dry_run(
    dry_runs: &std::collections::HashSet<(String, String)>,
    path: axum::extract::MatchedPath,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    let asked =
        req.uri().query().is_some_and(|q| url::form_urlencoded::parse(q.as_bytes()).any(|(k, _)| k == "dry_run"));
    if asked && req.method() != axum::http::Method::GET {
        let path = path.as_str().strip_prefix("/api/v1").unwrap_or(path.as_str());
        if !dry_runs.contains(&(req.method().as_str().to_ascii_lowercase(), path.to_owned())) {
            return ApiError::invalid("this change has no dry run: without dry_run it would be made").into_response();
        }
    }
    next.run(req).await
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

/// With `PGW_AUTH=oidc`, the endpoints pacod-id took over (signing in, passwords, links from
/// mails, accepting invitations) answer 410 with where to go instead.
pub fn moved_to_identity(st: &AdminState) -> Result<(), ApiError> {
    match &st.identity {
        Some(id) => Err(ApiError::new(StatusCode::GONE, "moved_to_identity", "this is done at pacod-id now")
            .with_details(serde_json::json!({ "code": "moved_to_identity", "issuer": id.issuer() }))),
        None => Ok(()),
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
