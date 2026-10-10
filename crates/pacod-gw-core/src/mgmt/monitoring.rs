//! Monitoring through the management API (concept sections 14.2 and 14.9): what the instances
//! are doing right now, the health of providers and models, and the alerts that are open. The
//! console needs no access to Prometheus for any of it.
//!
//! Every instance publishes its live figures in the instance registry (`cluster::report`).
//! The figures here add up all running instances, or show one with `?instance=`; the instance
//! that answers uses its own figures, which are current.

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::AdminState;
use super::auth::Principal;
use super::error::{ApiError, ApiResult};
use super::openapi::OpenApi;
use crate::cluster::report::{LiveReport, SecretChecks, merged_health};
use crate::cluster::{self, ALIVE_SECONDS, KEEP_STOPPED_HOURS};
use crate::model::{Doc, PriceStatus, RoleScope};
use crate::store::InstanceRow;
use crate::telemetry::live::{Health, LiveSnapshot, TrafficCounts};

/// What the running instances are doing now: their sum, or one instance's figures.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Live {
    /// The instance that answered.
    pub instance_id: String,
    /// `cluster`: the sum of all running instances; `instance`: the one asked for.
    pub scope: String,
    #[serde(flatten)]
    pub traffic: LiveSnapshot,
    /// The configuration version applied by the instance that answered (or the one asked
    /// for), and the most any instance is behind.
    pub config_version: i64,
    pub config_lag_versions: i64,
    /// Usage events and audit records waiting on disk for the analytics service, in bytes.
    pub spool_bytes: u64,
    pub audit_spool_bytes: u64,
    pub events_dropped: u64,
    pub audit_records_dropped: u64,
    /// The instances the figures cover.
    pub instances: Vec<InstanceLive>,
}

/// One running instance.
#[derive(Debug, Serialize, JsonSchema)]
pub struct InstanceLive {
    pub id: String,
    /// `all`, `gateway` or `admin`.
    pub role: String,
    pub hostname: String,
    pub software_version: String,
    pub applied_version: i64,
    pub started_at: DateTime<Utc>,
    pub heartbeat_at: DateTime<Utc>,
    /// Absent while the instance has not published figures, for example right after its
    /// start or when it runs an older version.
    pub figures: Option<InstanceFigures>,
}

/// One instance's own figures.
#[derive(Debug, Serialize, JsonSchema)]
pub struct InstanceFigures {
    pub taken_at: Option<DateTime<Utc>>,
    #[serde(flatten)]
    pub traffic: LiveSnapshot,
    pub config_version: i64,
    pub config_lag_versions: i64,
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
    /// `limit`, `price_missing`, `price_stale`, `price_proposal`, `exchange_rate`, `provider`,
    /// `spool`, `audit`, `configuration` or `instance`.
    pub kind: String,
    pub title: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organisation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// The instance the alert is about.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
}

fn alert(id: String, severity: &str, kind: &str, title: String, detail: String) -> Alert {
    Alert {
        id,
        severity: severity.into(),
        kind: kind.into(),
        title,
        detail,
        organisation_id: None,
        workspace_id: None,
        instance_id: None,
    }
}

fn platform_reader(p: &Principal) -> bool {
    p.is_platform_admin() || p.roles.iter().any(|(_, s)| *s == RoleScope::Platform)
}

#[derive(Debug, Default, Deserialize)]
struct InstanceQuery {
    /// Only this instance.
    instance: Option<String>,
}

/// A running instance with the figures it published, if they are recent.
struct Member {
    row: InstanceRow,
    report: Option<LiveReport>,
}

/// The running instances, this one first. Without the database only this one is known.
async fn members(st: &AdminState) -> Vec<Member> {
    let me = st.boot.instance_id.as_str();
    let now = Utc::now();
    let records = st.store.instance_records(ALIVE_SECONDS).await.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "reading the instance registry failed; showing this instance only");
        Vec::new()
    });
    // Another process presenting this instance's id is recorded on the stored row.
    let my_conflict = records.iter().find(|r| r.row.id == me).and_then(|r| r.row.conflict_at);
    let mut out: Vec<Member> = records
        .into_iter()
        .filter(|r| r.row.id != me)
        .map(|r| {
            let fresh = r.live_at.is_some_and(|t| (now - t).num_seconds() < ALIVE_SECONDS);
            let report = if fresh { r.live.as_ref().and_then(LiveReport::parse) } else { None };
            Member { row: r.row, report }
        })
        .collect();
    out.insert(
        0,
        Member {
            row: InstanceRow {
                id: me.to_owned(),
                role: st.boot.role.as_str().to_owned(),
                hostname: cluster::hostname(),
                software_version: env!("CARGO_PKG_VERSION").to_owned(),
                applied_version: st.config.version(),
                started_at: st.started_at,
                heartbeat_at: now,
                conflict_at: my_conflict,
            },
            report: Some(LiveReport::collect(&st.gw)),
        },
    );
    out
}

/// All members, or the one asked for.
fn select(members: Vec<Member>, instance: Option<&str>) -> ApiResult<Vec<Member>> {
    match instance {
        None => Ok(members),
        Some(id) => {
            let one: Vec<Member> = members.into_iter().filter(|m| m.row.id == id).collect();
            if one.is_empty() { Err(ApiError::not_found("instance")) } else { Ok(one) }
        }
    }
}

fn figures(r: &LiveReport) -> InstanceFigures {
    InstanceFigures {
        taken_at: r.taken_at,
        traffic: LiveSnapshot::from_counts(&r.traffic, r.in_flight as usize),
        config_version: r.config.version,
        config_lag_versions: r.config.lag,
        spool_bytes: r.spool.pending_bytes,
        audit_spool_bytes: r.audit_spool.pending_bytes,
        events_dropped: r.spool.dropped,
        audit_records_dropped: r.audit_spool.dropped,
    }
}

async fn live(State(st): State<AdminState>, p: Principal, Query(q): Query<InstanceQuery>) -> ApiResult<Json<Live>> {
    if !platform_reader(&p) {
        return Err(ApiError::forbidden());
    }
    let members = select(members(&st).await, q.instance.as_deref())?;
    let mut traffic = TrafficCounts::default();
    let mut in_flight = 0u64;
    let mut out = Live {
        instance_id: st.boot.instance_id.clone(),
        scope: if q.instance.is_some() { "instance" } else { "cluster" }.into(),
        traffic: LiveSnapshot::from_counts(&traffic, 0),
        config_version: st.config.version(),
        config_lag_versions: 0,
        spool_bytes: 0,
        audit_spool_bytes: 0,
        events_dropped: 0,
        audit_records_dropped: 0,
        instances: Vec::new(),
    };
    for m in &members {
        if let Some(r) = &m.report {
            traffic.add(&r.traffic);
            in_flight += r.in_flight;
            out.config_lag_versions = out.config_lag_versions.max(r.config.lag);
            out.spool_bytes += r.spool.pending_bytes;
            out.audit_spool_bytes += r.audit_spool.pending_bytes;
            out.events_dropped += r.spool.dropped;
            out.audit_records_dropped += r.audit_spool.dropped;
            if q.instance.is_some() {
                out.config_version = r.config.version;
            }
        }
    }
    out.traffic = LiveSnapshot::from_counts(&traffic, in_flight as usize);
    out.instances = members
        .iter()
        .map(|m| InstanceLive {
            id: m.row.id.clone(),
            role: m.row.role.clone(),
            hostname: m.row.hostname.clone(),
            software_version: m.row.software_version.clone(),
            applied_version: m.row.applied_version,
            started_at: m.row.started_at,
            heartbeat_at: m.row.heartbeat_at,
            figures: m.report.as_ref().map(figures),
        })
        .collect();
    Ok(Json(out))
}

/// What an instance knows about a model it routes to.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RoutingTarget {
    /// The instance whose view this is: breakers and calls in flight are kept per instance.
    pub instance_id: String,
    pub model: String,
    pub provider: String,
    /// The credential the state belongs to; `-` when the provider needs none.
    pub credential_id: String,
    /// `open`: calls go to other models first until the cool-down is over.
    pub breaker: crate::routing::BreakerState,
    pub in_flight: u32,
    /// Moving average of the time to the first byte. Unset before the first answer.
    pub first_byte_ms: Option<f64>,
}

async fn routing(
    State(st): State<AdminState>,
    p: Principal,
    Query(q): Query<InstanceQuery>,
) -> ApiResult<Json<Vec<RoutingTarget>>> {
    if !platform_reader(&p) {
        return Err(ApiError::forbidden());
    }
    let snap = st.config.snapshot();
    let members = select(members(&st).await, q.instance.as_deref())?;
    let out = members
        .iter()
        .flat_map(|m| m.report.iter().flat_map(move |r| r.routing.iter().map(move |t| (m, t))))
        // Every instance knows every pool member; one it has not called yet has learned
        // nothing about it. Only what an instance has used is shown.
        .filter(|(_, t)| {
            t.first_byte_ms.is_some() || t.in_flight > 0 || t.breaker != crate::routing::BreakerState::Closed
        })
        .map(|(m, t)| (m.row.id.as_str(), t))
        .filter_map(|(instance, t)| {
            let m = snap.models.get(&t.model_id)?;
            Some(RoutingTarget {
                instance_id: instance.to_owned(),
                model: m.doc.name.clone(),
                provider: snap.providers.get(&m.doc.provider_id).map(|p| p.doc.name.clone()).unwrap_or_default(),
                credential_id: t.credential_id.clone(),
                breaker: t.breaker,
                in_flight: t.in_flight,
                first_byte_ms: t.first_byte_ms,
            })
        })
        .collect();
    Ok(Json(out))
}

async fn health(
    State(st): State<AdminState>,
    p: Principal,
    Query(q): Query<InstanceQuery>,
) -> ApiResult<Json<Vec<Health>>> {
    if !platform_reader(&p) {
        return Err(ApiError::forbidden());
    }
    let members = select(members(&st).await, q.instance.as_deref())?;
    Ok(Json(merged_health(members.iter().filter_map(|m| m.report.as_ref()))))
}

/// The alerts that are open now, most severe first. Platform readers see all of them; others
/// see the limit alerts of the organisations and workspaces they can read.
async fn alerts(State(st): State<AdminState>, p: Principal) -> ApiResult<Json<Vec<Alert>>> {
    let snap = st.config.snapshot();
    let now = Utc::now();
    let all = platform_reader(&p);
    let mut out: Vec<Alert> = Vec::new();

    // --- limits nearly or fully used (covers spend caps) ---
    // From the stored totals, in one query: this instance may serve no traffic at all.
    let mut scopes = Vec::new();
    let mut applicable = Vec::new();
    for ws in snap.workspaces.values() {
        if !p.can_read_workspace(&ws.id, &ws.organisation_id) {
            continue;
        }
        let a = crate::limits::LimitEngine::scope_applicable(ws, now);
        scopes.push((ws, applicable.len()..applicable.len() + a.len()));
        applicable.extend(a);
    }
    let states = st.limits.stored_state(&st.store, &applicable).await?;
    for (ws, range) in scopes {
        let specs: std::collections::HashMap<&str, &crate::snapshot::LimitRt> =
            ws.limits.iter().map(|l| (l.id.as_str(), l.as_ref())).collect();
        for s in states[range].iter().cloned() {
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
        let mut unconverted: std::collections::BTreeMap<&str, Vec<&str>> = Default::default();
        for m in snap.models.values().filter(|m| m.doc.state.callable() && !m.doc.free) {
            if let Some(p) = snap.price_at(&m.id, now)
                && snap.settings.to_reporting(1.0, &p.currency).is_none()
            {
                unconverted.entry(p.currency.as_str()).or_default().push(m.doc.name.as_str());
            }
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
        for (currency, mut models) in unconverted {
            models.sort_unstable();
            out.push(alert(
                format!("exchange_rate:{currency}"),
                "warning",
                "exchange_rate",
                format!("No exchange rate for {currency}"),
                format!(
                    "Prices in {currency} ({}) cannot be converted into {}: reports and spend limits count their costs unconverted. Add the rate in the settings.",
                    models.join(", "),
                    snap.settings.reporting_currency
                ),
            ));
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

        let members = members(&st).await;
        let reporting = members.iter().filter(|m| m.report.is_some()).count();

        // --- providers and models, from the traffic of every instance ---
        let seen_from =
            if reporting > 1 { format!("across {reporting} instances") } else { "seen from this instance".into() };
        for h in merged_health(members.iter().filter_map(|m| m.report.as_ref())) {
            if h.calls >= 5 && h.success_ratio < 0.5 {
                out.push(alert(
                    format!("provider:{}:{}", h.provider, h.model),
                    if h.success_ratio == 0.0 { "critical" } else { "warning" },
                    "provider",
                    format!("{} of {} recent calls to {} failed", h.failed, h.calls, h.model),
                    format!("Provider \"{}\", last five minutes, {seen_from}.", h.provider),
                ));
            }
        }

        // --- each instance ---
        let mine = SecretChecks::of(&st.gw);
        for m in &members {
            out.extend(instance_alerts(m, &mine));
        }
        let stopped = st.store.instances_stopped(ALIVE_SECONDS, KEEP_STOPPED_HOURS).await.unwrap_or_default();
        for row in stopped {
            let mut a = alert(
                format!("instance_silent:{}", row.id),
                "warning",
                "instance",
                format!("The instance {} stopped reporting", row.id),
                format!(
                    "Its last heartbeat from host {} was at {} UTC. If it was stopped on purpose, it is forgotten after {KEEP_STOPPED_HOURS} hours.",
                    row.hostname,
                    row.heartbeat_at.format("%Y-%m-%d %H:%M:%S")
                ),
            );
            a.instance_id = Some(row.id);
            out.push(a);
        }

        // --- configuration ---
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

/// What is wrong with one running instance, judged by what it published.
fn instance_alerts(m: &Member, mine: &SecretChecks) -> Vec<Alert> {
    let id = &m.row.id;
    let mut out = Vec::new();
    let mut push = |key: &str, severity: &str, kind: &str, title: String, detail: String| {
        let mut a = alert(format!("{key}:{id}"), severity, kind, title, detail);
        a.instance_id = Some(id.clone());
        out.push(a);
    };
    if m.row.conflict_at.is_some_and(|t| (Utc::now() - t).num_seconds() < 60) {
        push(
            "instance_conflict",
            "critical",
            "instance",
            format!("Another process presented the instance id {id}"),
            "In the last minute a second process used this id while the instance ran. One that starts with it is refused; one already running keeps serving, but its figures are not shown and its events cannot be told apart. Give every instance its own PGW_INSTANCE_ID and spool directory.".into(),
        );
    }
    if m.row.software_version != env!("CARGO_PKG_VERSION") {
        push(
            "instance_version",
            "info",
            "instance",
            format!("The instance {id} runs version {}", m.row.software_version),
            format!("This instance runs {}. Expected during an upgrade.", env!("CARGO_PKG_VERSION")),
        );
    }
    let Some(r) = &m.report else { return out };
    if r.secrets.pepper != mine.pepper {
        push(
            "instance_secrets",
            "critical",
            "instance",
            format!("The instance {id} has a different PGW_KEY_PEPPER"),
            "It refuses every gateway key and admin token. Give it the same PGW_KEY_PEPPER as the other instances."
                .into(),
        );
    } else if (r.secrets.kek_id.as_str(), r.secrets.kek.as_str()) != (mine.kek_id.as_str(), mine.kek.as_str()) {
        push(
            "instance_secrets",
            "critical",
            "instance",
            format!("The instance {id} has a different key-encryption key"),
            "It may not be able to read the provider credentials. Give it the same PGW_KEK and PGW_KEK_ID as the other instances.".into(),
        );
    }
    if r.spool.dropped > 0 {
        push(
            "spool_dropped",
            "critical",
            "spool",
            format!("{} usage events were dropped on {id}", r.spool.dropped),
            "The event spool was full: the analytics service has been unreachable for too long.".into(),
        );
    } else if r.spool.enabled && r.spool.bytes > r.spool.max_bytes / 2 {
        push(
            "spool_backlog",
            "warning",
            "spool",
            format!("The event spool of {id} is more than half full"),
            "Usage events are waiting for the analytics service. Traffic is not affected yet.".into(),
        );
    }
    if r.audit_spool.dropped > 0 {
        push(
            "audit_dropped",
            "critical",
            "audit",
            format!("{} audit records were lost on {id}", r.audit_spool.dropped),
            "The audit spool was full or could not be written.".into(),
        );
    }
    if r.config.lag > 0 {
        push(
            "config_lag",
            "warning",
            "configuration",
            format!("The instance {id} has not applied the newest configuration"),
            format!("It is {} version(s) behind.", r.config.lag),
        );
    }
    out
}

/// How a model is doing, for the live feed: `failing` with at least five calls and fewer than
/// half successful in the last five minutes, `degraded` below nine in ten, `ok` otherwise.
fn health_class(h: &crate::telemetry::live::Health) -> &'static str {
    match h.success_ratio {
        _ if h.calls < 5 => "ok",
        r if r < 0.5 => "failing",
        r if r < 0.9 => "degraded",
        _ => "ok",
    }
}

/// Announces on the live feed when a model's health changes class (concept section 10.1),
/// from the figures every instance publishes. Runs until `shutdown` fires.
pub async fn watch_health(st: AdminState, mut shutdown: tokio::sync::watch::Receiver<bool>) {
    let mut last: std::collections::HashMap<(String, String), &'static str> = std::collections::HashMap::new();
    loop {
        tokio::select! {
            () = tokio::time::sleep(std::time::Duration::from_secs(st.boot.upkeep_seconds.clamp(1, 15))) => {}
            _ = shutdown.changed() => return,
        }
        if st.feed.receiver_count() == 0 {
            continue;
        }
        let members = members(&st).await;
        for h in merged_health(members.iter().filter_map(|m| m.report.as_ref())) {
            let class = health_class(&h);
            let key = (h.provider.clone(), h.model.clone());
            let previous = last.insert(key, class).unwrap_or("ok");
            if previous != class {
                st.announce(
                    "health",
                    serde_json::json!({
                        "provider": h.provider, "model": h.model, "status": class, "previous": previous,
                        "calls": h.calls, "success_ratio": h.success_ratio,
                    }),
                );
            }
        }
    }
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let live_schema = api.schema::<Live>();
    let health_schema = api.schema::<Health>();
    let alert_schema = api.schema::<Alert>();
    let array = |item: serde_json::Value| serde_json::json!({ "type": "array", "items": item });
    const INSTANCE: &str = "Only this instance. Default: all running instances.";
    api.add(
        api.op(
            "get",
            "/monitoring/live",
            "monitoringLive",
            "monitoring",
            "What the running instances are doing right now",
        )
        .query("instance", INSTANCE)
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
        .query("instance", INSTANCE)
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
    let routing_schema = api.schema::<RoutingTarget>();
    api.add(
        api.op(
            "get",
            "/monitoring/routing",
            "monitoringRouting",
            "monitoring",
            "Circuit breakers, calls in flight and time to first byte of the models each instance has called",
        )
        .query("instance", INSTANCE)
        .ok(array(routing_schema)),
    );
    router
        .route("/monitoring/routing", get(routing))
        .route("/monitoring/live", get(live))
        .route("/monitoring/health", get(health))
        .route("/monitoring/alerts", get(alerts))
}
