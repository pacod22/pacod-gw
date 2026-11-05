//! The scraper's loop: it asks the core for the check's and the scan's lease twice a minute,
//! runs what is due while a heartbeat keeps the run's lease, and reports what it found. The core
//! decides when a run is due, what it covers and what happens with the findings.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use pacod_gw_events::extraction::PageReading;
use pacod_gw_events::ingest::INTERNAL_TOKEN_HEADER;
use pacod_gw_events::jobs::{
    AgentStatus, CheckWork, HeartbeatReply, HeartbeatRequest, JOB_PRICE_CHECK, JOB_PRICE_SCAN, JobResult, LeaseReply,
    LeaseRequest, ScanReport, ScanWork,
};
use serde_json::{Value, json};
use tokio::sync::watch;

use crate::fetch::Fetcher;
use crate::render::Render;

/// How often the scraper asks for work.
pub const POLL: Duration = Duration::from_secs(30);
/// How long a lease is held without a heartbeat. Short, so that a scraper that stops frees its
/// run soon; the heartbeat renews it while the run lasts.
pub const LEASE_SECONDS: u32 = 180;
/// How often a run's lease is renewed.
pub const HEARTBEAT: Duration = Duration::from_secs(60);

/// The core's internal job API.
#[derive(Clone)]
pub struct Core {
    base: String,
    token: String,
    http: reqwest::Client,
}

impl Core {
    pub fn new(base: &str, token: &str) -> reqwest::Result<Self> {
        let http =
            reqwest::Client::builder().user_agent(concat!("pacod-gw-scraper/", env!("CARGO_PKG_VERSION"))).build()?;
        Ok(Self { base: base.trim_end_matches('/').to_owned(), token: token.to_owned(), http })
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> anyhow::Result<T> {
        let resp = self
            .http
            .post(format!("{}/api/v1/internal/jobs{path}", self.base))
            .header(INTERNAL_TOKEN_HEADER, &self.token)
            .json(body)
            // A reading may wait for the extraction model, in several parts.
            .timeout(Duration::from_secs(600))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("the core answered {status}: {}", text.chars().take(300).collect::<String>());
        }
        Ok(resp.json().await?)
    }

    pub async fn lease(&self, job: &str, holder: &str, agent: AgentStatus) -> anyhow::Result<LeaseReply> {
        let req =
            LeaseRequest { job: job.into(), holder: holder.into(), ttl_seconds: LEASE_SECONDS, agent: Some(agent) };
        self.post("/lease", &req).await
    }

    pub async fn heartbeat(&self, run_id: &str, holder: &str) -> anyhow::Result<bool> {
        let req = HeartbeatRequest { holder: holder.into(), ttl_seconds: LEASE_SECONDS };
        Ok(self.post::<HeartbeatReply>(&format!("/{run_id}/heartbeat"), &req).await?.keep_going)
    }

    pub async fn reading(&self, run_id: &str, reading: &PageReading) -> anyhow::Result<Value> {
        self.post(&format!("/{run_id}/readings"), reading).await
    }

    pub async fn scan_report(&self, run_id: &str, report: &ScanReport) -> anyhow::Result<Value> {
        self.post(&format!("/{run_id}/scan"), report).await
    }

    pub async fn finish(
        &self,
        run_id: &str,
        job: &str,
        ok: bool,
        message: String,
        result: Value,
    ) -> anyhow::Result<()> {
        let report =
            JobResult { run_id: run_id.into(), job: job.into(), finished_at: chrono::Utc::now(), ok, message, result };
        let _: Value = self.post(&format!("/{run_id}/result"), &report).await?;
        Ok(())
    }
}

/// What the scraper runs with.
pub struct Runner {
    pub core: Core,
    /// Names this instance as the holder of a lease. New at every start, so that a run left
    /// by a scraper that stopped is the core's to close and run again.
    pub holder: String,
    pub fetch: Fetcher,
    pub render: Arc<dyn Render>,
}

impl Runner {
    pub fn new(core: Core, fetch: Fetcher, render: Arc<dyn Render>) -> Self {
        Self { core, holder: format!("scraper-{}", uuid::Uuid::now_v7().simple()), fetch, render }
    }

    async fn agent(&self) -> AgentStatus {
        AgentStatus {
            service: "gw-scraper".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            browser: Some(self.render.status().await),
        }
    }

    /// Asks for one job's lease, and runs it when it is due. Returns whether a run happened.
    pub async fn tick(self: &Arc<Self>, job: &str) -> anyhow::Result<bool> {
        let lease = self.core.lease(job, &self.holder, self.agent().await).await?;
        let (true, true, Some(run_id)) = (lease.granted, lease.due, lease.run_id) else { return Ok(false) };
        tracing::info!(job, run_id, "price tracker run started");
        let keep = Arc::new(AtomicBool::new(true));
        let (stop, stopped) = watch::channel(false);
        let beat = tokio::spawn(heartbeat(self.clone(), run_id.clone(), keep.clone(), stopped));
        let outcome = match job {
            JOB_PRICE_CHECK => {
                let work: CheckWork = serde_json::from_value(lease.work)?;
                crate::check::run(self, &run_id, &work, &keep).await
            }
            JOB_PRICE_SCAN => {
                let work: ScanWork = serde_json::from_value(lease.work)?;
                crate::scan::run(self, &run_id, &work, &keep).await
            }
            other => Err(anyhow::anyhow!("no such job: {other}")),
        };
        let _ = stop.send(true);
        let _ = beat.await;
        if !keep.load(Ordering::Relaxed) {
            // The run is another scraper's now, or over: its record is not this one's to write.
            tracing::warn!(job, run_id, "the core took the run back; it was left unfinished here");
            return Ok(true);
        }
        let (ok, message, result) = match outcome {
            Ok(result) => (true, String::new(), result),
            Err(e) => (false, format!("{e:#}"), json!({})),
        };
        self.core.finish(&run_id, job, ok, message, result).await?;
        tracing::info!(job, run_id, ok, "price tracker run finished");
        Ok(true)
    }

    /// Runs until `shutdown` turns true. A failure to reach the core is logged when it starts
    /// and when it ends, not every round: the core being away is the core's matter, but an
    /// operator must see that the scraper cannot work.
    pub async fn run_forever(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let mut failing: HashMap<&str, String> = HashMap::new();
        loop {
            for job in [JOB_PRICE_CHECK, JOB_PRICE_SCAN] {
                match self.tick(job).await {
                    Ok(_) => {
                        if failing.remove(job).is_some() {
                            tracing::info!(job, "the core answers again");
                        }
                    }
                    Err(e) => {
                        let error = format!("{e:#}");
                        if failing.get(job) != Some(&error) {
                            tracing::warn!(job, %error, "asking the core for work failed; trying again every 30 s");
                            failing.insert(job, error);
                        }
                    }
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(POLL) => {}
                _ = shutdown.changed() => return,
            }
        }
    }
}

/// Renews a run's lease every minute until the run ends. When the core says the run is no
/// longer this scraper's, `keep` turns false and the run stops at its next page.
async fn heartbeat(runner: Arc<Runner>, run_id: String, keep: Arc<AtomicBool>, mut stopped: watch::Receiver<bool>) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(HEARTBEAT) => {}
            _ = stopped.changed() => return,
        }
        match runner.core.heartbeat(&run_id, &runner.holder).await {
            Ok(true) => {}
            Ok(false) => {
                keep.store(false, Ordering::Relaxed);
                return;
            }
            // The core may be away for a moment; the lease outlasts two missed beats.
            Err(e) => tracing::warn!(run_id, error = %format!("{e:#}"), "renewing a run's lease failed"),
        }
    }
}
