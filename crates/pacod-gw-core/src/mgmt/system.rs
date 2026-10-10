//! System status, the audit log, global settings and the live event feed.

use std::convert::Infallible;

use axum::extract::{Query, State};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::get;
use axum::{Json, Router};
use futures_util::StreamExt;
use futures_util::stream::Stream;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio_stream::wrappers::BroadcastStream;

use super::AdminState;
use super::auth::Principal;
use super::crud::Page;
use super::error::{ApiError, ApiResult};
use super::openapi::{OpenApi, page_schema};
use crate::model::settings::GLOBAL_SETTINGS_ID;
use crate::model::{Doc, GlobalSettings};
use crate::store::{AuditRow, Change, InstanceRow};

/// The state of one part of the installation.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Component {
    pub name: String,
    /// `ok`, `down` or `not_configured`.
    pub status: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SystemStatus {
    pub software_version: String,
    pub instance_id: String,
    pub role: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
    /// The configuration version this instance has applied.
    pub config_version: i64,
    /// The newest configuration version in the database.
    pub config_head: i64,
    /// Running instances and the version each has applied.
    pub instances: Vec<InstanceRow>,
    /// How many instances have applied `config_head`.
    pub instances_current: usize,
    /// Instances that stopped heart-beating without signing off in the last day, most recent
    /// first: crashed, killed, or cut off from the database.
    pub stopped_instances: Vec<InstanceRow>,
    pub components: Vec<Component>,
    /// Configuration objects that could not be used.
    pub problems: Vec<String>,
}

async fn status(State(st): State<AdminState>, p: Principal) -> ApiResult<Json<SystemStatus>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    let head = st.store.head_version().await?;
    let instances = st.store.instances(crate::cluster::ALIVE_SECONDS).await?;
    let stopped_instances =
        st.store.instances_stopped(crate::cluster::ALIVE_SECONDS, crate::cluster::KEEP_STOPPED_HOURS).await?;
    let mut components = vec![Component { name: "database".into(), status: "ok".into(), detail: String::new() }];
    components.push(match &st.boot.analytics_url {
        None => Component {
            name: "analytics".into(),
            status: "not_configured".into(),
            detail: "reports, audit mode and the price tracker are unavailable".into(),
        },
        Some(url) => {
            let ok = st
                .http
                .get(format!("{url}/readyz"))
                .timeout(std::time::Duration::from_secs(3))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success());
            Component {
                name: "analytics".into(),
                status: if ok { "ok" } else { "down" }.into(),
                detail: if ok { String::new() } else { "events accumulate in the spool until it returns".into() },
            }
        }
    });
    Ok(Json(SystemStatus {
        software_version: env!("CARGO_PKG_VERSION").into(),
        instance_id: st.boot.instance_id.clone(),
        role: st.boot.role.as_str().into(),
        started_at: st.started_at,
        config_version: snap.version,
        config_head: head,
        instances_current: instances.iter().filter(|i| i.applied_version >= head).count(),
        instances,
        stopped_instances,
        components,
        problems: snap.problems.clone(),
    }))
}

#[derive(Debug, Deserialize)]
struct LimitQuery {
    limit: Option<i64>,
}

async fn config_versions(
    State(st): State<AdminState>,
    p: Principal,
    Query(q): Query<LimitQuery>,
) -> ApiResult<Json<Vec<crate::store::Change>>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    Ok(Json(st.store.recent_changes(q.limit.unwrap_or(100).clamp(1, 1000)).await?))
}

/// Whether the running instances have applied a configuration version yet.
#[derive(Debug, Serialize, JsonSchema)]
pub struct VersionApplied {
    pub version: i64,
    /// How many running instances have applied it.
    pub applied: usize,
    /// How many instances are running.
    pub of: usize,
}

async fn config_version(
    State(st): State<AdminState>,
    p: Principal,
    axum::extract::Path(version): axum::extract::Path<i64>,
) -> ApiResult<Json<VersionApplied>> {
    if !p.has_any_platform_role() {
        return Err(ApiError::forbidden());
    }
    let (applied, of) = st.applied(version).await;
    Ok(Json(VersionApplied { version, applied, of }))
}

#[derive(Debug, Deserialize)]
struct AuditQuery {
    cursor: Option<i64>,
    limit: Option<i64>,
    organisation_id: Option<String>,
    workspace_id: Option<String>,
}

async fn audit_log(
    State(st): State<AdminState>,
    p: Principal,
    Query(q): Query<AuditQuery>,
) -> ApiResult<Json<Page<AuditRow>>> {
    // Without an organisation filter the whole log is returned, which only platform-level
    // readers may see.
    if !p.can_read_audit_log(q.organisation_id.as_deref()) {
        return Err(ApiError::forbidden());
    }
    let limit = q.limit.unwrap_or(100).clamp(1, 500);
    let mut rows =
        st.store.audit_list(q.cursor, limit + 1, q.organisation_id.as_deref(), q.workspace_id.as_deref()).await?;
    let more = rows.len() as i64 > limit;
    rows.truncate(limit as usize);
    let next_cursor = if more { rows.last().map(|r| r.id.to_string()) } else { None };
    Ok(Json(Page { items: rows, next_cursor }))
}

async fn get_settings(State(st): State<AdminState>, _p: Principal) -> Json<GlobalSettings> {
    Json(st.config.snapshot().settings.clone())
}

async fn put_settings(
    State(st): State<AdminState>,
    p: Principal,
    Json(input): Json<GlobalSettings>,
) -> ApiResult<Json<GlobalSettings>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    if input.reporting_currency.len() != 3 || !input.reporting_currency.bytes().all(|b| b.is_ascii_uppercase()) {
        return Err(ApiError::invalid("reporting_currency must be a three-letter code such as USD"));
    }
    if !(1..=24 * 30).contains(&input.session_hours) {
        return Err(ApiError::invalid("session_hours must be between 1 and 720"));
    }
    if !(1..=365).contains(&input.remember_me_days) {
        return Err(ApiError::invalid("remember_me_days must be between 1 and 365"));
    }
    if !(1..=24 * 7).contains(&input.verification_link_hours) {
        return Err(ApiError::invalid("verification_link_hours must be between 1 and 168"));
    }
    if !(5..=24 * 60).contains(&input.reset_link_minutes) {
        return Err(ApiError::invalid("reset_link_minutes must be between 5 and 1440"));
    }
    if !(1..=30).contains(&input.invitation_link_days) {
        return Err(ApiError::invalid("invitation_link_days must be between 1 and 30"));
    }
    if !(1024..=256 << 20).contains(&input.audit_max_body_bytes) {
        return Err(ApiError::invalid("audit_max_body_bytes must be between 1024 and 268435456"));
    }
    if input.discovery_interval_hours > 24 * 90 {
        return Err(ApiError::invalid("discovery_interval_hours must be at most 2160; 0 runs it only when asked"));
    }
    if input.usage_retention_days.is_some_and(|d| d > 36_500) {
        return Err(ApiError::invalid("usage_retention_days must be at most 36500; 0 keeps every event"));
    }
    if input.tag_keys.len() > 8 || input.tag_keys.iter().any(|k| k.trim().is_empty() || k.len() > 64) {
        return Err(ApiError::invalid("tag_keys holds up to eight tag keys"));
    }
    if let Some(a) = &input.audit
        && (a.rule.is_some() || a.sample.is_some_and(|x| !(x > 0.0 && x <= 1.0)))
    {
        return Err(ApiError::invalid(
            "audit holds the default audit mode; its sample must be above 0 and at most 1, and the rule is audit_rule",
        ));
    }
    let b = input.breaker;
    if !(1..=100).contains(&b.failures) {
        return Err(ApiError::invalid("breaker.failures must be between 1 and 100"));
    }
    if !(1..=3600).contains(&b.cooldown_seconds) || !(b.cooldown_seconds..=86_400).contains(&b.max_cooldown_seconds) {
        return Err(ApiError::invalid(
            "breaker.cooldown_seconds must be between 1 and 3600, and max_cooldown_seconds between it and 86400",
        ));
    }
    let value = serde_json::to_value(&input).map_err(ApiError::internal)?;
    let mut tx = st.store.begin().await?;
    if tx.get(GLOBAL_SETTINGS_ID).await?.is_some() {
        tx.update(GLOBAL_SETTINGS_ID, None, None, &value, p.audit(None, None)).await?;
    } else {
        tx.create(GlobalSettings::KIND, GLOBAL_SETTINGS_ID, None, None, &value, p.audit(None, None)).await?;
    }
    st.commit(tx).await?;
    Ok(Json(input))
}

/// Kinds of global objects that every signed-in user may read, so they hear of changes.
const READABLE_BY_ALL: &[&str] = &["model"];

/// Whether a principal may hear of a change: one of its organisations or workspaces, a global
/// object everyone may read, or anything at all for platform roles.
fn may_see(p: &Principal, c: &Change) -> bool {
    if c.op == "reload" {
        return true;
    }
    match (&c.organisation_id, &c.workspace_id) {
        (Some(org), Some(ws)) => p.can_read_workspace(ws, org),
        (Some(org), None) => p.can_read_org(org),
        (None, _) => p.has_any_platform_role() || READABLE_BY_ALL.contains(&c.kind.as_str()),
    }
}

/// The live feed: one event for every configuration change this instance applies that the
/// caller may read, and for platform roles the progress of jobs and changes of model health.
async fn events(State(st): State<AdminState>, p: Principal) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let platform = p.has_any_platform_role();
    let changes = BroadcastStream::new(st.config.subscribe()).filter_map(move |item| {
        futures_util::future::ready(match item {
            Ok(change) if may_see(&p, &change) => {
                Some(Ok(Event::default().event("config").json_data(&change).unwrap_or_default()))
            }
            Ok(_) => None,
            // The receiver fell behind: tell the client to refresh everything.
            Err(_) => Some(Ok(Event::default().event("config").data(r#"{"kind":"*","object_id":"*","op":"reload"}"#))),
        })
    });
    let feed = BroadcastStream::new(st.feed.subscribe()).filter_map(move |item| {
        futures_util::future::ready(match item {
            Ok(e) if platform => Some(Ok(Event::default().event(e.event).json_data(&e.data).unwrap_or_default())),
            _ => None,
        })
    });
    Sse::new(futures_util::stream::select(changes, feed)).keep_alive(KeepAlive::default())
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let status_schema = api.schema::<SystemStatus>();
    let audit = api.schema::<AuditRow>();
    let settings = api.schema::<GlobalSettings>();
    api.add(
        api.op(
            "get",
            "/system/status",
            "systemStatus",
            "system",
            "Instances, applied configuration version and component health",
        )
        .ok(status_schema),
    );
    let applied_schema = api.schema::<VersionApplied>();
    api.add(
        api.op(
            "get",
            "/system/config-versions/{version}",
            "configVersionApplied",
            "system",
            "How many running instances have applied a configuration version",
        )
        .ok(applied_schema),
    );
    api.add(
        api.op(
            "get",
            "/system/config-versions",
            "configVersions",
            "system",
            "Recent configuration changes, newest first",
        )
        .query_int("limit", "How many changes, default 100.")
        .ok(serde_json::json!({ "type": "array", "items": {
                "type": "object",
                "required": ["version", "kind", "object_id", "op"],
                "properties": {
                    "version": { "type": "integer" }, "kind": { "type": "string" },
                    "object_id": { "type": "string" }, "op": { "type": "string" }
                }
            } })),
    );
    api.add(
        api.op("get", "/audit-log", "auditLog", "system", "What administrators did, newest first")
            .query("cursor", "Continue a listing.")
            .query_int("limit", "Page size, default 100.")
            .query("organisation_id", "Restrict to one organisation.")
            .query("workspace_id", "Restrict to one workspace.")
            .ok(page_schema(audit)),
    );
    api.add(api.op("get", "/settings", "getSettings", "system", "Installation-wide settings").ok(settings.clone()));
    api.add(
        api.op("put", "/settings", "putSettings", "system", "Replace the installation-wide settings")
            .body(settings.clone())
            .ok(settings),
    );
    api.add(
        api.op(
            "get",
            "/events",
            "events",
            "system",
            "Server-sent events: `config` for every configuration change the caller may read; for platform roles also `job` (price and discovery runs) and `health` (a model's health changed)",
        )
        .response("200", "An event stream", None),
    );
    let rewrap = api.schema::<crate::mgmt::resources::provider::RewrapReport>();
    api.add(
        api.op(
            "post",
            "/system/rewrap-secrets",
            "rewrapSecrets",
            "system",
            "Wrap every stored credential under the current key-encryption key, after a rotation (platform admins)",
        )
        .ok(rewrap),
    );
    router
        .route("/system/status", get(status))
        .route("/system/config-versions", get(config_versions))
        .route("/system/config-versions/{version}", get(config_version))
        .route("/audit-log", get(audit_log))
        .route("/settings", get(get_settings).put(put_settings))
        .route("/events", get(events))
        .route("/system/rewrap-secrets", axum::routing::post(rewrap_secrets))
}

async fn rewrap_secrets(
    State(st): State<AdminState>,
    p: Principal,
) -> ApiResult<Json<crate::mgmt::resources::provider::RewrapReport>> {
    if !p.is_platform_admin() {
        return Err(ApiError::forbidden());
    }
    let mut tx = st.store.begin().await?;
    let report = crate::mgmt::resources::provider::rewrap_credentials(&mut tx, &st.boot.keks, &p.audit(None, None))
        .await
        .map_err(ApiError::internal)?;
    st.commit(tx).await?;
    Ok(Json(report))
}
