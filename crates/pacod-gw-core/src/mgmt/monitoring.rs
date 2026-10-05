//! Monitoring through the management API (concept sections 14.2 and 14.9): what this instance
//! is doing right now, the health of providers and models, and the alerts that are open. The
//! console needs no access to Prometheus for any of it.

use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use chrono::Utc;
use schemars::JsonSchema;
use serde::Serialize;

use super::AdminState;
use super::auth::Principal;
use super::error::{ApiError, ApiResult};
use super::openapi::OpenApi;
use crate::model::{Doc, PriceStatus, RoleScope};
use crate::telemetry::live::{Health, LiveSnapshot};

/// The current figures of this instance.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Live {
    pub instance_id: String,
    #[serde(flatten)]
    pub traffic: LiveSnapshot,
    /// The configuration version this instance has applied, and how far it is behind.
    pub config_version: i64,
    pub config_lag_versions: i64,
    /// Usage events and audit records waiting on disk for the analytics service, in bytes.
    pub spool_bytes: u64,
    pub audit_spool_bytes: u64,
    pub events_dropped: u64,
    pub audit_records_dropped: u64,
}

/// Something an operator should look at.
#[derive(Debug, Clone, Serialize, JsonSchema, PartialEq)]
pub struct Alert {
    /// Stable for as long as the condition lasts.
    pub id: String,
    /// `critical`, `warning` or `info`.
    pub severity: String,
    /// `limit`, `price_missing`, `price_stale`, `price_proposal`, `provider`, `spool`, `audit`
    /// or `configuration`.
    pub kind: String,
    pub title: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organisation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

fn alert(id: String, severity: &str, kind: &str, title: String, detail: String) -> Alert {
    Alert { id, severity: severity.into(), kind: kind.into(), title, detail, organisation_id: None, workspace_id: None }
}

fn platform_reader(p: &Principal) -> bool {
    p.is_platform_admin() || p.roles.iter().any(|(_, s)| *s == RoleScope::Platform)
}

async fn live(State(st): State<AdminState>, p: Principal) -> ApiResult<Json<Live>> {
    if !platform_reader(&p) {
        return Err(ApiError::forbidden());
    }
    let gw = &st.gw;
    Ok(Json(Live {
        instance_id: st.boot.instance_id.clone(),
        traffic: st.live.snapshot(),
        config_version: st.config.version(),
        config_lag_versions: st.config.lag(),
        spool_bytes: gw.usage_spool.pending_bytes(),
        audit_spool_bytes: gw.audit_spool.pending_bytes(),
        events_dropped: gw.usage_spool.dropped(),
        audit_records_dropped: gw.audit_spool.dropped(),
    }))
}

async fn health(State(st): State<AdminState>, p: Principal) -> ApiResult<Json<Vec<Health>>> {
    if !platform_reader(&p) {
        return Err(ApiError::forbidden());
    }
    Ok(Json(st.live.health()))
}

/// The alerts that are open now, most severe first. Platform readers see all of them; others
/// see the limit alerts of the organisations and workspaces they can read.
async fn alerts(State(st): State<AdminState>, p: Principal) -> ApiResult<Json<Vec<Alert>>> {
    let snap = st.config.snapshot();
    let now = Utc::now();
    let all = platform_reader(&p);
    let mut out: Vec<Alert> = Vec::new();

    // --- limits nearly or fully used (covers spend caps) ---
    for ws in snap.workspaces.values() {
        if !p.can_read_workspace(&ws.id, &ws.organisation_id) {
            continue;
        }
        let specs: std::collections::HashMap<&str, &crate::snapshot::LimitRt> =
            ws.limits.iter().map(|l| (l.id.as_str(), l.as_ref())).collect();
        for s in st.limits.scope_states(ws, now) {
            let Some(limit) = specs.get(s.limit_id.as_str()) else { continue };
            let percent = if s.amount > 0.0 { s.used / s.amount * 100.0 } else { 100.0 };
            // The limit's own thresholds; without any, four fifths is worth a look.
            let mut thresholds: Vec<f64> = limit.spec.alert_thresholds.iter().map(|t| f64::from(*t)).collect();
            if thresholds.is_empty() {
                thresholds.push(80.0);
            }
            let Some(reached) = thresholds.iter().copied().filter(|t| percent >= *t).reduce(f64::max) else { continue };
            // An organisation's limit shows once, not once per workspace.
            let org_wide = limit.owner_id == ws.organisation_id;
            let id = format!("limit:{}", s.limit_id);
            if out.iter().any(|a| a.id == id) {
                continue;
            }
            let mut a = alert(
                id,
                if percent >= 100.0 { "critical" } else { "warning" },
                "limit",
                format!("The limit \"{}\" is at {:.0} %", s.name, percent),
                format!("{} of {} used; the threshold of {:.0} % is reached.", s.used, s.amount, reached),
            );
            a.organisation_id = Some(ws.organisation_id.clone());
            a.workspace_id = (!org_wide).then(|| ws.id.clone());
            out.push(a);
        }
    }

    if all {
        // --- prices ---
        let stale_days = i64::from(snap.price_job.stale_days);
        for m in snap.models.values().filter(|m| m.doc.state.callable() && !m.doc.free) {
            match snap.price_at(&m.id, now) {
                None => out.push(alert(
                    format!("price_missing:{}", m.id),
                    "critical",
                    "price_missing",
                    format!("The model \"{}\" has no price", m.doc.name),
                    "Its calls are recorded without a cost. Set a price, or mark the model as free.".into(),
                )),
                Some(p) if (now - p.checked_at()).num_days() > stale_days => out.push(alert(
                    format!("price_stale:{}", m.id),
                    "info",
                    "price_stale",
                    format!(
                        "The price of \"{}\" was last checked {} days ago",
                        m.doc.name,
                        (now - p.checked_at()).num_days()
                    ),
                    format!("It has not changed or been confirmed for more than {stale_days} days."),
                )),
                Some(_) => {}
            }
        }
        let rows = st.store.list(crate::model::Price::KIND, crate::store::ParentFilter::Any, None, 5000).await?;
        let proposed = rows.iter().filter(|r| r.doc["status"] == serde_json::json!(PriceStatus::Proposed)).count();
        if proposed > 0 {
            out.push(alert(
                "price_proposals".into(),
                "info",
                "price_proposal",
                format!(
                    "{proposed} price proposal{} waiting for approval",
                    if proposed == 1 { " is" } else { "s are" }
                ),
                "The price tracker found changes it may not apply on its own.".into(),
            ));
        }

        // --- providers and models ---
        for h in st.live.health() {
            if h.calls >= 5 && h.success_ratio < 0.5 {
                out.push(alert(
                    format!("provider:{}:{}", h.provider, h.model),
                    if h.success_ratio == 0.0 { "critical" } else { "warning" },
                    "provider",
                    format!("{} of {} recent calls to {} failed", h.failed, h.calls, h.model),
                    format!("Provider \"{}\", last five minutes, seen from this instance.", h.provider),
                ));
            }
        }

        // --- this instance ---
        let gw = &st.gw;
        if gw.usage_spool.dropped() > 0 {
            out.push(alert(
                "spool_dropped".into(),
                "critical",
                "spool",
                format!("{} usage events were dropped", gw.usage_spool.dropped()),
                "The event spool was full: the analytics service has been unreachable for too long.".into(),
            ));
        } else if gw.usage_spool.enabled() && gw.usage_spool.bytes() > st.boot.spool_max_bytes / 2 {
            out.push(alert(
                "spool_backlog".into(),
                "warning",
                "spool",
                "The event spool is more than half full".into(),
                "Usage events are waiting for the analytics service. Traffic is not affected yet.".into(),
            ));
        }
        if gw.audit_spool.dropped() > 0 {
            out.push(alert(
                "audit_dropped".into(),
                "critical",
                "audit",
                format!("{} audit records were lost", gw.audit_spool.dropped()),
                "The audit spool was full or could not be written.".into(),
            ));
        }
        if st.config.lag() > 0 {
            out.push(alert(
                "config_lag".into(),
                "warning",
                "configuration",
                "This instance has not applied the newest configuration".into(),
                format!("It is {} version(s) behind.", st.config.lag()),
            ));
        }
        for (i, problem) in snap.problems.iter().enumerate() {
            out.push(alert(
                format!("config_problem:{i}"),
                "warning",
                "configuration",
                "A configuration object cannot be used".into(),
                problem.clone(),
            ));
        }
    }

    let rank = |s: &str| match s {
        "critical" => 0,
        "warning" => 1,
        _ => 2,
    };
    out.sort_by(|a, b| rank(&a.severity).cmp(&rank(&b.severity)).then_with(|| a.id.cmp(&b.id)));
    Ok(Json(out))
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let live_schema = api.schema::<Live>();
    let health_schema = api.schema::<Health>();
    let alert_schema = api.schema::<Alert>();
    let array = |item: serde_json::Value| serde_json::json!({ "type": "array", "items": item });
    api.add(
        api.op("get", "/monitoring/live", "monitoringLive", "monitoring", "What this instance is doing right now")
            .ok(live_schema),
    );
    api.add(
        api.op(
            "get",
            "/monitoring/health",
            "monitoringHealth",
            "monitoring",
            "Health of providers and models from live traffic, worst first",
        )
        .ok(array(health_schema)),
    );
    api.add(
        api.op(
            "get",
            "/monitoring/alerts",
            "monitoringAlerts",
            "monitoring",
            "The alerts that are open, most severe first",
        )
        .ok(array(alert_schema)),
    );
    router
        .route("/monitoring/live", get(live))
        .route("/monitoring/health", get(health))
        .route("/monitoring/alerts", get(alerts))
}
