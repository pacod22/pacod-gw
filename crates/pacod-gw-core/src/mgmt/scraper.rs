//! The core's side of the scraper (`gw-scraper`): the leases of the daily check and the weekly
//! scan, the work each run is handed, the heartbeat that keeps a long run's lease, and what the
//! console is told about the scraper. The scraper renders pages and looks for them; the core
//! decides what is read and what happens with what is found.

use std::collections::{BTreeSet, HashSet};

use axum::Json;
use axum::extract::{Path, State};
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use pacod_gw_events::jobs::{
    CheckWork, HeartbeatReply, HeartbeatRequest, JOB_PRICE_CHECK, JOB_PRICE_SCAN, JOB_PRICES, JobResult, LeaseReply,
    LeaseRequest, PageTask, ScanProvider, ScanRoot, ScanWork,
};
use schemars::JsonSchema;
use serde::Serialize;
use serde_json::{Value, json};

use crate::mgmt::AdminState;
use crate::mgmt::error::{ApiError, ApiResult};
use crate::model::pricejob::{due_by, next_after, parse_cron};
use crate::model::{ModelState, PageKind, PageStatus, PriceJob, Status, new_id, preset_for_base_url};
use crate::snapshot::Snapshot;
use crate::store::jobs::JobRun;

/// The message a run is closed with when the scraper that ran it stopped.
pub const LEFT_OPEN: &str = "the scraper that ran it stopped before it finished";

/// How long after its lease ran out upkeep closes a run nobody took over.
const LAPSED_MINUTES: i64 = 10;

/// How recently a scraper must have asked for work to count as running.
const PRESENT_SECONDS: i64 = 300;

/// A value of a snake-case enum as its name, for the job messages.
fn name_of(v: &impl Serialize) -> String {
    serde_json::to_value(v).ok().and_then(|v| v.as_str().map(str::to_owned)).unwrap_or_default()
}

/// Whether the core has a model to read pages with.
pub fn can_extract(job: &PriceJob) -> bool {
    job.extraction_model.is_some() || job.extraction.is_some()
}

/// The provider's ids of the repository's models at a provider, retired ones left out.
fn known_models(snap: &Snapshot, provider_id: &str) -> Vec<String> {
    let ids: BTreeSet<String> = snap
        .models
        .values()
        .filter(|m| m.doc.provider_id == provider_id && m.doc.state != ModelState::Retired)
        .map(|m| m.doc.upstream_id.clone())
        .collect();
    ids.into_iter().collect()
}

// --- what a run covers ------------------------------------------------------------------------

/// The pages a check covers. A scheduled or full run reads every active page. A run that was
/// asked for reads the pages asked for, and every page of a provider whose models (`models`)
/// wait for a price, read even when unchanged.
async fn check_pages(st: &AdminState, snap: &Snapshot, models: Option<&[String]>) -> ApiResult<Vec<String>> {
    let active = || snap.price_pages.iter().filter(|p| p.doc.status == PageStatus::Active);
    let Some(models) = models else { return Ok(active().map(|p| p.id.clone()).collect()) };
    let providers: HashSet<&str> =
        models.iter().filter_map(|m| snap.models.get(m)).map(|m| m.doc.provider_id.as_str()).collect();
    let waiting: Vec<(String, String)> = active()
        .filter(|p| providers.contains(p.doc.provider_id.as_str()))
        .map(|p| (p.id.clone(), p.doc.provider_id.clone()))
        .collect();
    st.store.price_pages_request_check(&waiting, true).await?;
    let states = st.store.price_page_states().await?;
    Ok(active().filter(|p| states.get(&p.id).is_some_and(|s| s.check_requested)).map(|p| p.id.clone()).collect())
}

/// The work of a check run over `pages`. A page deleted or dismissed since the run started is
/// left out.
async fn check_work(st: &AdminState, snap: &Snapshot, pages: &[String]) -> ApiResult<CheckWork> {
    let states = st.store.price_page_states().await?;
    let tasks = pages
        .iter()
        .filter_map(|id| snap.price_pages.iter().find(|p| &p.id == id && p.doc.status == PageStatus::Active))
        .map(|p| {
            let state = states.get(&p.id);
            PageTask {
                page_id: p.id.clone(),
                provider_id: p.doc.provider_id.clone(),
                provider: snap.providers.get(&p.doc.provider_id).map(|r| r.doc.name.clone()).unwrap_or_default(),
                url: p.doc.url.clone(),
                kind: name_of(&p.doc.kind),
                format: name_of(&p.doc.format),
                key_prefix: p.doc.key_prefix.clone(),
                previous_hash: state.map(|s| s.content_hash.clone()).unwrap_or_default(),
                // A page never read is read whatever its fingerprint.
                force: state.is_none_or(|s| s.force_read || s.last_read_at.is_none()),
                known_models: known_models(snap, &p.doc.provider_id),
            }
        })
        .collect();
    Ok(CheckWork { pages: tasks, extraction: can_extract(&snap.price_job) })
}

/// Where the scan starts for a provider: its active pages and the pages its preset names,
/// except those it has a page at already that is not active (proposed, or dismissed by an
/// administrator, whose word stands).
fn scan_provider(snap: &Snapshot, provider_id: &str) -> Option<ScanProvider> {
    let provider = snap.providers.get(provider_id)?;
    let pages: Vec<_> = snap.price_pages.iter().filter(|p| p.doc.provider_id == provider_id).collect();
    let mut roots: Vec<ScanRoot> = pages
        .iter()
        .filter(|p| p.doc.status == PageStatus::Active && p.doc.kind == PageKind::Page)
        .map(|p| ScanRoot { url: p.doc.url.clone(), page_id: Some(p.id.clone()), seed: false })
        .collect();
    let seeds = preset_for_base_url(&provider.doc.base_url).map(|p| p.price_pages).unwrap_or_default();
    for seed in seeds {
        let Ok(url) = pacod_gw_events::pages::normalise_url(seed) else { continue };
        if !pages.iter().any(|p| p.doc.url == url) {
            roots.push(ScanRoot { url, page_id: None, seed: true });
        }
    }
    (!roots.is_empty()).then(|| ScanProvider {
        provider_id: provider_id.to_owned(),
        name: provider.doc.name.clone(),
        roots,
        known_urls: pages.iter().map(|p| p.doc.url.clone()).collect(),
        known_models: known_models(snap, provider_id),
    })
}

/// The providers a scan covers: the active ones with somewhere to start, or those asked for.
fn scan_providers(snap: &Snapshot, asked: Option<&[String]>) -> Vec<String> {
    let mut ids: Vec<String> = snap
        .providers
        .values()
        .filter(|p| p.doc.status == Status::Active)
        .filter(|p| asked.is_none_or(|a| a.is_empty() || a.contains(&p.id)))
        .filter(|p| scan_provider(snap, &p.id).is_some())
        .map(|p| p.id.clone())
        .collect();
    ids.sort();
    ids
}

fn scan_work(snap: &Snapshot, providers: &[String]) -> ScanWork {
    let s = &snap.price_job.scan;
    ScanWork {
        providers: providers.iter().filter_map(|id| scan_provider(snap, id)).collect(),
        max_pages_per_provider: s.max_pages_per_provider,
        min_score: s.min_score,
        keywords: s.keywords.clone(),
    }
}

// --- leases and runs --------------------------------------------------------------------------

async fn start(st: &AdminState, job: &str, trigger: &str, full: bool, covers: &[String]) -> ApiResult<String> {
    let id = new_id("run");
    st.store.job_start_covering(job, &id, trigger, full, covers).await?;
    st.announce("job", json!({ "job": job, "run_id": id, "status": "started", "trigger": trigger }));
    Ok(id)
}

async fn close_left_open(st: &AdminState, job: &str, run_id: &str) -> Result<(), crate::store::StoreError> {
    if st.store.job_finish_run(run_id, false, LEFT_OPEN, &json!({})).await? {
        tracing::warn!(job, run_id, "a run of the price tracker was left open by a scraper that stopped; closed");
        st.announce("job", json!({ "job": job, "run_id": run_id, "status": "failed", "message": LEFT_OPEN }));
    }
    Ok(())
}

/// Gives out the lease of the check or the scan and, when one is due, the work of a run.
pub async fn lease(st: &AdminState, req: &LeaseRequest) -> ApiResult<Json<LeaseReply>> {
    let nothing = |granted| Json(LeaseReply { granted, run_id: None, due: false, work: Value::Null });
    let ttl = req.ttl_seconds.clamp(30, 3600);
    st.store.job_polled(&req.job, Some(&json!({ "holder": req.holder, "agent": req.agent }))).await?;
    let before = st.store.job_state(&req.job).await?;
    if !st.store.job_lease(&req.job, &req.holder, ttl).await? {
        return Ok(nothing(false));
    }
    let snap = st.config.snapshot();
    let check = req.job == JOB_PRICE_CHECK;
    let state = st.store.job_state(&req.job).await?;
    let (run_id, covers) = match state.current_run {
        // The holder's own run, asked for again: a scraper that restarted under the same name.
        Some(open) if before.holder == req.holder => {
            let covers = st.store.job_run(&open).await?.and_then(|r| r.models).unwrap_or_default();
            (open, covers)
        }
        // Another holder's run, whose lease ran out: it stopped. The run is closed as failed and
        // run once more, unless it was the second try already. Pages read before have their
        // fingerprint stored, so the second try does not have them read again.
        Some(open) => {
            let left = st.store.job_run(&open).await?;
            close_left_open(st, &req.job, &open).await?;
            // Closing a run gives up its lease; this holder takes it again.
            if !st.store.job_lease(&req.job, &req.holder, ttl).await? {
                return Ok(nothing(false));
            }
            let covers = match left {
                Some(r) if r.trigger != "retry" => r.models.unwrap_or_default(),
                _ => return Ok(nothing(true)),
            };
            if covers.is_empty() {
                return Ok(nothing(true));
            }
            (start(st, &req.job, "retry", false, &covers).await?, covers)
        }
        None => {
            let job = &snap.price_job;
            let schedule = if check { &job.schedule } else { &job.scan_schedule };
            let due = job.enabled && due_by(schedule, &job.time_zone, state.last_started_at, Utc::now());
            if !due && !state.run_requested {
                return Ok(nothing(true));
            }
            // A scheduled run covers everything, also what was asked for alone.
            let scope = if due { None } else { state.run_models.clone() };
            let covers = if check {
                check_pages(st, &snap, scope.as_deref()).await?
            } else {
                scan_providers(&snap, scope.as_deref())
            };
            if covers.is_empty() {
                st.store.job_skip(&req.job, scope.is_none()).await?;
                return Ok(nothing(true));
            }
            let trigger = match (due, &scope) {
                (true, _) => "schedule",
                (false, None) => "manual",
                // Pages asked for alone, or the pages of models waiting for their price.
                (false, Some(m)) if check && m.is_empty() => "page",
                (false, Some(_)) if check => "activation",
                (false, Some(_)) => "manual",
            };
            (start(st, &req.job, trigger, scope.is_none(), &covers).await?, covers)
        }
    };
    let work = if check {
        serde_json::to_value(check_work(st, &snap, &covers).await?)
    } else {
        serde_json::to_value(scan_work(&snap, &covers))
    }
    .map_err(ApiError::internal)?;
    Ok(Json(LeaseReply { granted: true, run_id: Some(run_id), due: true, work }))
}

/// Keeps a run's lease while the scraper works on it. Answers `keep_going: false` when the run
/// is over or another holder has it, so that the scraper stops.
pub async fn heartbeat(
    State(st): State<AdminState>,
    headers: HeaderMap,
    Path(run_id): Path<String>,
    Json(req): Json<HeartbeatRequest>,
) -> ApiResult<Json<HeartbeatReply>> {
    super::resources::pricejob::internal(&st, &headers)?;
    let run = st.store.job_run(&run_id).await?.ok_or_else(|| ApiError::not_found("run"))?;
    let state = st.store.job_state(&run.job).await?;
    let mine = run.finished_at.is_none()
        && state.current_run.as_deref() == Some(run_id.as_str())
        && state.holder == req.holder;
    let keep_going = mine && st.store.job_lease(&run.job, &req.holder, req.ttl_seconds.clamp(30, 3600)).await?;
    Ok(Json(HeartbeatReply { keep_going }))
}

/// Closes a run of the check or the scan: what the scraper reports is laid over what the core
/// recorded while the run went on.
pub async fn finish(st: &AdminState, run: JobRun, res: JobResult) -> ApiResult<Json<Value>> {
    let mut result = if run.result.is_object() { run.result } else { json!({}) };
    if let (Some(into), Some(from)) = (result.as_object_mut(), res.result.as_object()) {
        into.extend(from.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    st.store.job_finish_run(&run.id, res.ok, &res.message, &result).await?;
    st.announce(
        "job",
        json!({ "job": run.job, "run_id": run.id, "status": if res.ok { "finished" } else { "failed" } }),
    );
    Ok(Json(json!({ "run_id": run.id })))
}

/// Closes the runs of the check and the scan that nobody took over long after their lease ran
/// out. A run left open keeps the job from starting another; the next lease would close it too,
/// but only when a scraper asks.
pub async fn close_lapsed(st: &AdminState) -> anyhow::Result<()> {
    let lapsed = Utc::now() - chrono::Duration::minutes(LAPSED_MINUTES);
    for job in [JOB_PRICE_CHECK, JOB_PRICE_SCAN] {
        let state = st.store.job_state(job).await?;
        if let (Some(open), Some(until)) = (state.current_run, state.lease_until)
            && until < lapsed
        {
            close_left_open(st, job, &open).await?;
        }
    }
    // A run of the price job of earlier releases that its analytics service left open: nothing
    // hands it out any more.
    if let Some(open) = st.store.job_state(JOB_PRICES).await?.current_run {
        st.store.job_finish_run(&open, false, super::resources::pricejob::EARLIER_JOB, &json!({})).await?;
    }
    Ok(())
}

// --- what the console is told -----------------------------------------------------------------

/// One of the scraper's jobs as the console shows it.
#[derive(Debug, Serialize, JsonSchema)]
pub struct JobView {
    pub schedule: String,
    pub last_started_at: Option<DateTime<Utc>>,
    /// The next scheduled start. Absent while the tracker is off.
    pub next_run_at: Option<DateTime<Utc>>,
    /// A run was asked for and has not started yet.
    pub run_requested: bool,
    pub running: bool,
    /// The last run that finished.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run: Option<JobRun>,
}

/// The scraper's two jobs.
#[derive(Debug, Serialize, JsonSchema)]
pub struct JobViews {
    pub check: JobView,
    pub scan: JobView,
}

/// The browser the scraper renders pages in, as the scraper last reported it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct BrowserView {
    pub ok: bool,
    pub product: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub message: String,
}

/// Whether a scraper runs, as far as the core can tell: it asks for work every half minute.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ScraperView {
    /// When a scraper last asked for work.
    pub last_seen_at: Option<DateTime<Utc>>,
    /// Whether one asked in the last five minutes.
    pub present: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserView>,
}

async fn job_view(st: &AdminState, job: &PriceJob, name: &str) -> ApiResult<(JobView, crate::store::jobs::JobState)> {
    let state = st.store.job_state(name).await?;
    let schedule = if name == JOB_PRICE_CHECK { &job.schedule } else { &job.scan_schedule };
    let next_run_at = job
        .enabled
        .then(|| next_after(schedule, &job.time_zone, state.last_started_at.unwrap_or_else(Utc::now)))
        .flatten();
    let view = JobView {
        schedule: schedule.clone(),
        last_started_at: state.last_started_at,
        next_run_at,
        run_requested: state.run_requested,
        running: state.current_run.is_some(),
        last_run: st.store.job_last_finished(name).await?,
    };
    Ok((view, state))
}

/// The check, the scan and the scraper, for the price job's view.
pub async fn views(st: &AdminState, job: &PriceJob) -> ApiResult<(JobView, JobView, ScraperView)> {
    let (check, a) = job_view(st, job, JOB_PRICE_CHECK).await?;
    let (scan, b) = job_view(st, job, JOB_PRICE_SCAN).await?;
    let latest = [a, b].into_iter().filter(|s| s.last_polled_at.is_some()).max_by_key(|s| s.last_polled_at);
    let last_seen_at = latest.as_ref().and_then(|s| s.last_polled_at);
    let agent = latest.and_then(|s| s.poll_status).map(|p| p["agent"].clone()).unwrap_or(Value::Null);
    let browser = agent.get("browser").filter(|b| b.is_object()).map(|b| BrowserView {
        ok: b["ok"].as_bool().unwrap_or(false),
        product: b["product"].as_str().unwrap_or_default().to_owned(),
        message: b["message"].as_str().unwrap_or_default().to_owned(),
    });
    let scraper = ScraperView {
        last_seen_at,
        present: last_seen_at.is_some_and(|t| (Utc::now() - t).num_seconds() <= PRESENT_SECONDS),
        version: agent["version"].as_str().map(str::to_owned),
        browser,
    };
    Ok((check, scan, scraper))
}

/// The providers whose pages the tracker reads.
pub fn tracked_providers(snap: &Snapshot) -> Vec<String> {
    let ids: BTreeSet<String> = snap
        .price_pages
        .iter()
        .filter(|p| p.doc.status == PageStatus::Active)
        .map(|p| p.doc.provider_id.clone())
        .collect();
    ids.into_iter().collect()
}

/// What keeps the tracker from doing its work, in sentences for the console.
pub fn problems(st: &AdminState, snap: &Snapshot, job: &PriceJob, scraper: &ScraperView) -> Vec<String> {
    let mut out = Vec::new();
    for (field, schedule) in [("schedule", &job.schedule), ("scan_schedule", &job.scan_schedule)] {
        if let Err(e) = parse_cron(schedule) {
            out.push(format!("The {field} \"{schedule}\" cannot be read, so it never runs: {e}."));
        }
    }
    if !job.enabled {
        return out;
    }
    if st.boot.internal_token.is_empty() {
        out.push("PGW_INTERNAL_TOKEN is not set, so the scraper cannot ask for work.".into());
    }
    if !scraper.present {
        out.push(
            "No scraper has asked for work in the last five minutes: is gw-scraper running, with PGW_CORE_ADMIN_URL \
             and PGW_INTERNAL_TOKEN set?"
                .into(),
        );
    } else if let Some(b) = scraper.browser.as_ref().filter(|b| !b.ok) {
        out.push(format!("The scraper cannot reach its browser: {}.", b.message.trim_end_matches('.')));
    }
    let active = snap.price_pages.iter().filter(|p| p.doc.status == PageStatus::Active);
    if !can_extract(job) && active.clone().any(|p| p.doc.kind == PageKind::Page) {
        out.push("No extraction model is set: pages are checked, but nothing reads them.".into());
    }
    if active.count() == 0 {
        out.push("No page is confirmed for any provider: confirm the pages the scan proposes, or add some.".into());
    }
    out
}
