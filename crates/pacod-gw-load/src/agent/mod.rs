//! The load agent: runs one part of a load test for the tester and reports it second by
//! second (`pacod-gw-load-agent`).
//!
//! The tester pushes a plan (`POST /v1/runs`), the agent checks it, warms its connections
//! and makes one probe call; the tester then starts every agent of the run at once and asks
//! each for its seconds about once a second. Every request needs the token both share.
//!
//! One run at a time. A run lives on a lease: every report renews it, and an agent that is
//! not asked for `lease_s` seconds stops on its own, so a tester that went away (restarted,
//! lost its network) does not leave load running against a gateway.

pub mod check;
pub mod config;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use parking_lot::Mutex;
use serde::Deserialize;
use serde_json::json;
use subtle::ConstantTimeEq;
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

use crate::client::{self, CallOutcome, ErrorKind, Ready};
use crate::contract::{
    Busy, CONTRACT, Ended, ErrorBody, ErrorDetail, Info, Mode, Prepare, Prepared, Probe, Report, RunState,
};
use crate::engine::{self, Job, Live, Stop};
use config::{AgentConfig, token_digest};

/// Bytes of a failed probe's answer kept for the tester: enough for the gateway's reason.
const EXCERPT: usize = 500;
/// How long a warm-up connection may take.
const WARMUP_TIMEOUT: Duration = Duration::from_secs(5);

/// The agent and its one run slot.
pub struct Agent {
    cfg: AgentConfig,
    version: String,
    slot: Mutex<Option<Slot>>,
    /// Set while a prepare is warming up and probing, so that a second one is refused.
    preparing: AtomicBool,
}

/// The current or last run.
struct Slot {
    run_id: String,
    live: Arc<Live>,
    stop: watch::Sender<Option<Stop>>,
    /// The work, until the run starts.
    job: Option<Arc<Job>>,
    seen: Instant,
    lease: Duration,
    error: String,
}

impl Slot {
    fn state(&self) -> RunState {
        self.live.state().0
    }
}

impl Agent {
    pub fn new(cfg: AgentConfig) -> Arc<Agent> {
        Arc::new(Agent {
            cfg,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            slot: Mutex::new(None),
            preparing: AtomicBool::new(false),
        })
    }

    pub fn name(&self) -> &str {
        &self.cfg.settings.name
    }

    /// The request's bearer token against the configured one, compared as digests in
    /// constant time so that neither its content nor its length leaks.
    fn authorised(&self, headers: &HeaderMap) -> bool {
        let presented = headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .map(str::trim)
            .unwrap_or("");
        bool::from(token_digest(presented).ct_eq(&self.cfg.token_digest)) && !presented.is_empty()
    }

    fn busy(&self) -> Option<Busy> {
        let slot = self.slot.lock();
        let s = slot.as_ref()?;
        let state = s.state();
        (!state.finished()).then(|| Busy { run_id: s.run_id.clone(), state })
    }

    fn info(&self) -> Info {
        Info {
            contract: CONTRACT,
            name: self.cfg.settings.name.clone(),
            version: self.version.clone(),
            target: self.cfg.settings.target.as_str().trim_end_matches('/').to_owned(),
            caps: self.cfg.settings.caps.clone(),
            cpus: self.cfg.cpus,
            busy: self.busy(),
        }
    }

    /// Stops a running or prepared run (shutdown); the answer is its live state to wait on.
    pub fn stop_all(&self) -> Option<Arc<Live>> {
        let mut slot = self.slot.lock();
        let s = slot.as_mut()?;
        if s.state().finished() {
            return None;
        }
        if s.job.take().is_some() {
            s.live.set_state(RunState::Cancelled, Some(Ended::Cancelled));
            return None;
        }
        s.stop.send_replace(Some(Stop::Cancel));
        Some(s.live.clone())
    }

    /// Ends runs whose lease has run out. Called once a second by [`spawn_lease_keeper`].
    fn enforce_lease(&self) {
        let mut slot = self.slot.lock();
        let Some(s) = slot.as_mut() else { return };
        if s.state().finished() || s.seen.elapsed() <= s.lease {
            return;
        }
        if s.job.take().is_some() {
            tracing::warn!(run_id = %s.run_id, "the tester did not start the prepared run in time; dropped");
            s.live.set_state(RunState::Cancelled, Some(Ended::LeaseExpired));
        } else if s.stop.borrow().is_none() {
            tracing::warn!(run_id = %s.run_id, "no report asked for within the lease; stopping the run");
            s.stop.send_replace(Some(Stop::LeaseExpired));
        }
    }
}

/// Checks the leases once a second while the agent runs.
pub fn spawn_lease_keeper(agent: Arc<Agent>) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut every = tokio::time::interval(Duration::from_secs(1));
        loop {
            every.tick().await;
            agent.enforce_lease();
        }
    })
}

/// The agent's HTTP routes.
pub fn router(agent: Arc<Agent>) -> Router {
    // The request body is JSON-escaped inside the plan, which can make it up to twice as long.
    let limit = (agent.cfg.settings.caps.max_body_bytes as usize).saturating_mul(2).saturating_add(64 * 1024);
    Router::new()
        .route("/healthz", get(|| async { Json(json!({ "status": "ok" })) }))
        .route("/v1/info", get(info))
        .route("/v1/runs", post(prepare))
        .route("/v1/runs/{id}/start", post(start))
        .route("/v1/runs/{id}/report", get(report))
        .route("/v1/runs/{id}/cancel", post(cancel))
        .layer(DefaultBodyLimit::max(limit))
        .with_state(agent)
}

fn problem(status: StatusCode, code: &str, message: &str) -> Response {
    let body = ErrorBody { error: ErrorDetail { code: code.to_owned(), message: message.to_owned() } };
    (status, Json(body)).into_response()
}

fn unauthorized() -> Response {
    problem(StatusCode::UNAUTHORIZED, "unauthorized", "a valid agent token is required")
}

fn state_name(state: RunState) -> &'static str {
    match state {
        RunState::Prepared => "prepared",
        RunState::Running => "running",
        RunState::Draining => "draining",
        RunState::Done => "done",
        RunState::Cancelled => "cancelled",
        RunState::Failed => "failed",
    }
}

async fn info(State(agent): State<Arc<Agent>>, headers: HeaderMap) -> Response {
    if !agent.authorised(&headers) {
        return unauthorized();
    }
    Json(agent.info()).into_response()
}

/// Clears [`Agent::preparing`] however a prepare ends.
struct Preparing<'a>(&'a AtomicBool);

impl Drop for Preparing<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

async fn prepare(State(agent): State<Arc<Agent>>, headers: HeaderMap, body: Bytes) -> Response {
    if !agent.authorised(&headers) {
        return unauthorized();
    }
    let p: Prepare = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => return problem(StatusCode::UNPROCESSABLE_ENTITY, "invalid", &format!("the plan is not valid: {e}")),
    };
    let base = match check::check(&p, &agent.cfg.settings.caps, &agent.cfg.settings.target) {
        Ok(base) => base,
        Err(e) => return problem(StatusCode::UNPROCESSABLE_ENTITY, "invalid", &e),
    };
    if agent.preparing.swap(true, Ordering::AcqRel) {
        return problem(StatusCode::CONFLICT, "busy", "another run is being prepared");
    }
    let _preparing = Preparing(&agent.preparing);
    if let Some(b) = agent.busy() {
        return problem(StatusCode::CONFLICT, "busy", &format!("run {} is {}", b.run_id, state_name(b.state)));
    }

    let pool = match p.profile.mode {
        Mode::Concurrency => p.profile.peak().ceil() as usize,
        Mode::Rate => p.profile.max_in_flight.unwrap_or(1) as usize,
    };
    let http = match client::build(&base, p.profile.http, pool, agent.cfg.ca_pem.as_deref()) {
        Ok(http) => http,
        Err(e) => return problem(StatusCode::INTERNAL_SERVER_ERROR, "failed", &format!("{e:#}")),
    };
    let ready = match Ready::new(&base, &p.request, p.auth_header, &p.key) {
        Ok(ready) => ready,
        Err(e) => return problem(StatusCode::UNPROCESSABLE_ENTITY, "invalid", &format!("{e:#}")),
    };
    warm_up(&http, &base, p.profile.warmup_connections).await;
    let timeout = Duration::from_secs(p.profile.timeout_s);
    let probe = probe(client::call(&http, &ready, timeout, EXCERPT).await, &p.key);

    // A refused key stops here: a load of wrong keys would get the agent's address blocked
    // (60 failures a minute) and measure nothing.
    let refused = matches!(probe.status, Some(401 | 403));
    let live = Live::new();
    let (stop, _) = watch::channel(None);
    let (state, job, error) = if refused {
        live.set_state(RunState::Failed, Some(Ended::Error));
        let status = probe.status.unwrap_or_default();
        (RunState::Failed, None, format!("the gateway refused the key ({status}); no load was made"))
    } else {
        let job = Job { http, ready, profile: p.profile.clone(), timeout };
        (RunState::Prepared, Some(Arc::new(job)), String::new())
    };
    tracing::info!(
        run_id = %p.run_id,
        part = p.part,
        parts = p.parts,
        base = %base,
        mode = ?p.profile.mode,
        peak = p.profile.peak(),
        duration_s = p.profile.duration_s(),
        max_calls = ?p.profile.max_calls,
        probe_status = ?probe.status,
        "prepared"
    );
    *agent.slot.lock() = Some(Slot {
        run_id: p.run_id.clone(),
        live,
        stop,
        job,
        seen: Instant::now(),
        lease: Duration::from_secs(p.lease_s),
        error,
    });
    (StatusCode::CREATED, Json(Prepared { run_id: p.run_id, state, probe })).into_response()
}

/// Opens `n` connections before the start, so that the first second does not measure their
/// set-up. The answers do not matter: a gateway other than pacod-gw may have no `/healthz`.
async fn warm_up(http: &reqwest::Client, base: &url::Url, n: u32) {
    let Ok(url) = client::join(base, "/healthz") else { return };
    let mut set = JoinSet::new();
    for _ in 0..n {
        let request = http.get(url.clone()).timeout(WARMUP_TIMEOUT);
        set.spawn(async move {
            if let Ok(resp) = request.send().await {
                let _ = resp.bytes().await;
            }
        });
    }
    while set.join_next().await.is_some() {}
}

fn probe(o: CallOutcome, key: &str) -> Probe {
    let excerpt = if o.ok() {
        String::new()
    } else {
        let text = String::from_utf8_lossy(&o.excerpt);
        // A gateway may echo what it was sent; the key never leaves the agent.
        let text = if key.is_empty() { text.into_owned() } else { text.replace(key, "[redacted]") };
        text.chars().take(EXCERPT).collect()
    };
    Probe {
        status: o.status,
        ttfb_ms: o.ttfb_us as f64 / 1000.0,
        total_ms: o.total_us as f64 / 1000.0,
        error: o.error.map(|k| {
            match k {
                ErrorKind::Connect => "could not connect",
                ErrorKind::Timeout => "timed out",
                ErrorKind::Body => "the answer broke off",
                ErrorKind::Other => "the call failed",
            }
            .to_owned()
        }),
        body_excerpt: excerpt,
    }
}

async fn start(State(agent): State<Arc<Agent>>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    if !agent.authorised(&headers) {
        return unauthorized();
    }
    let (job, live, rx) = {
        let mut slot = agent.slot.lock();
        let Some(s) = slot.as_mut().filter(|s| s.run_id == id) else {
            return problem(StatusCode::NOT_FOUND, "not_found", &format!("no run {id} here"));
        };
        let Some(job) = s.job.take() else {
            return problem(StatusCode::CONFLICT, "busy", &format!("run {id} is {}", state_name(s.state())));
        };
        s.seen = Instant::now();
        s.live.set_state(RunState::Running, None);
        (job, s.live.clone(), s.stop.subscribe())
    };
    let name = agent.name().to_owned();
    tokio::spawn(async move {
        let ended = engine::run(job, live.clone(), rx).await;
        let state = match ended {
            Ended::Duration | Ended::MaxCalls => RunState::Done,
            Ended::Cancelled | Ended::LeaseExpired => RunState::Cancelled,
            Ended::Error => RunState::Failed,
        };
        live.set_state(state, Some(ended));
        tracing::info!(run_id = %id, agent = %name, ?ended, calls = live.calls(), "run ended");
    });
    (StatusCode::ACCEPTED, Json(json!({ "state": "running" }))).into_response()
}

#[derive(Deserialize)]
struct After {
    after: Option<u64>,
}

async fn report(
    State(agent): State<Arc<Agent>>,
    Path(id): Path<String>,
    Query(q): Query<After>,
    headers: HeaderMap,
) -> Response {
    if !agent.authorised(&headers) {
        return unauthorized();
    }
    let (live, error) = {
        let mut slot = agent.slot.lock();
        let Some(s) = slot.as_mut().filter(|s| s.run_id == id) else {
            return problem(StatusCode::NOT_FOUND, "not_found", &format!("no run {id} here"));
        };
        s.seen = Instant::now();
        (s.live.clone(), s.error.clone())
    };
    let (state, ended) = live.state();
    Json(Report {
        run_id: id,
        state,
        ended,
        error,
        elapsed_ms: live.elapsed().as_millis() as u64,
        in_flight: live.in_flight(),
        calls: live.calls(),
        intervals: live.intervals_after(q.after),
    })
    .into_response()
}

async fn cancel(State(agent): State<Arc<Agent>>, Path(id): Path<String>, headers: HeaderMap) -> Response {
    if !agent.authorised(&headers) {
        return unauthorized();
    }
    let mut slot = agent.slot.lock();
    let Some(s) = slot.as_mut().filter(|s| s.run_id == id) else {
        return problem(StatusCode::NOT_FOUND, "not_found", &format!("no run {id} here"));
    };
    if s.job.take().is_some() {
        s.live.set_state(RunState::Cancelled, Some(Ended::Cancelled));
    } else if !s.state().finished() {
        s.stop.send_replace(Some(Stop::Cancel));
    }
    let state = s.state();
    let said = if state.finished() { state_name(state) } else { "cancelling" };
    (StatusCode::ACCEPTED, Json(json!({ "state": said }))).into_response()
}
