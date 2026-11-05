//! Batch jobs on the stored data: retention and the rollups. A job runs under a lease from the
//! core's internal job API (`pacod_gw_events::jobs`), so that one analytics instance runs it at a
//! time. The core decides when a run is due and records it.
//!
//! The price tracker ran here in earlier releases; it is gw-scraper's now (crates/pacod-gw-scraper),
//! which renders the providers' pages. This service fetches nothing from outside any more.

pub mod retention;
pub mod rollups;

use std::time::Duration;

use pacod_gw_events::ingest::INTERNAL_TOKEN_HEADER;
use pacod_gw_events::jobs::{JOB_RETENTION, JOB_ROLLUPS, JobResult, LeaseReply, LeaseRequest};

use crate::storage::ClickHouse;

/// How often an instance asks the core for work.
const POLL: Duration = Duration::from_secs(30);

/// How long a lease is held. Long enough for a run; a crashed instance frees it by not renewing.
const LEASE_SECONDS: u32 = 600;

/// What the job runner needs to reach the core and the stored data.
#[derive(Clone)]
pub struct Runner {
    /// The core's management API, for example `http://pacod-gw-admin:8081`.
    pub core: String,
    pub internal_token: String,
    /// Names this instance as the holder of a lease.
    pub holder: String,
    /// For the core, which is on the internal network by design.
    pub http: reqwest::Client,
    /// For the jobs that work on the stored data: retention and rollups.
    pub clickhouse: Option<ClickHouse>,
}

impl Runner {
    pub fn new(core: &str, internal_token: &str) -> anyhow::Result<Self> {
        Ok(Self {
            core: core.trim_end_matches('/').to_owned(),
            internal_token: internal_token.to_owned(),
            holder: format!("analytics-{}", uuid::Uuid::now_v7().simple()),
            http: reqwest::Client::builder()
                .user_agent(concat!("pacod-gw-analytics/", env!("CARGO_PKG_VERSION")))
                .build()?,
            clickhouse: None,
        })
    }

    /// Lets the runner work on the stored data too.
    pub fn with_clickhouse(mut self, clickhouse: ClickHouse) -> Self {
        self.clickhouse = Some(clickhouse);
        self
    }

    async fn post<T: serde::de::DeserializeOwned>(
        &self,
        path: &str,
        body: &impl serde::Serialize,
    ) -> anyhow::Result<T> {
        let resp = self
            .http
            .post(format!("{}/api/v1/internal/jobs{path}", self.core))
            .header(INTERNAL_TOKEN_HEADER, &self.internal_token)
            .json(body)
            // An extraction is an LLM call and may take a while.
            .timeout(Duration::from_secs(300))
            .send()
            .await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("the core answered {status}: {}", text.chars().take(300).collect::<String>());
        }
        Ok(resp.json().await?)
    }

    /// Asks for the retention job once, and runs it when it is due. Returns whether a run
    /// happened.
    pub async fn tick_retention(&self) -> anyhow::Result<bool> {
        let Some(clickhouse) = &self.clickhouse else { return Ok(false) };
        let lease: LeaseReply = self
            .post(
                "/lease",
                &LeaseRequest {
                    job: JOB_RETENTION.into(),
                    holder: self.holder.clone(),
                    ttl_seconds: LEASE_SECONDS,
                    agent: None,
                },
            )
            .await?;
        let (true, true, Some(run_id)) = (lease.granted, lease.due, lease.run_id) else { return Ok(false) };
        tracing::info!(run_id, "retention run started");
        let outcome = retention::run(clickhouse, &lease.work, chrono::Utc::now()).await;
        let (ok, message, result) = match outcome {
            Ok(done) => (true, String::new(), done),
            Err(e) => (false, format!("{e:#}"), serde_json::json!({})),
        };
        let report = JobResult {
            run_id: run_id.clone(),
            job: JOB_RETENTION.into(),
            finished_at: chrono::Utc::now(),
            ok,
            message,
            result,
        };
        let _: serde_json::Value = self.post(&format!("/{run_id}/result"), &report).await?;
        tracing::info!(run_id, ok, "retention run finished");
        Ok(true)
    }

    /// Asks for the rollup job once, and rebuilds the rollups when it is due.
    pub async fn tick_rollups(&self) -> anyhow::Result<bool> {
        let Some(clickhouse) = &self.clickhouse else { return Ok(false) };
        let lease: LeaseReply = self
            .post(
                "/lease",
                &LeaseRequest {
                    job: JOB_ROLLUPS.into(),
                    holder: self.holder.clone(),
                    ttl_seconds: LEASE_SECONDS,
                    agent: None,
                },
            )
            .await?;
        if !(lease.granted && lease.due) {
            return Ok(false);
        }
        let done = rollups::run(clickhouse, chrono::Utc::now()).await?;
        tracing::debug!(%done, "rollups rebuilt");
        Ok(true)
    }

    /// Runs until the task is aborted.
    pub async fn run_forever(self) {
        // A failure is logged when it starts and when it ends, not every round: the core being
        // away is the core's matter, but an operator must see that retention does not run.
        let mut failing: Option<String> = None;
        loop {
            match self.tick_retention().await {
                Ok(_) if failing.take().is_some() => tracing::info!("the core answers the retention job again"),
                Ok(_) => {}
                Err(error) => {
                    let error = format!("{error:#}");
                    if failing.as_ref() != Some(&error) {
                        tracing::warn!(%error, "asking the core for the retention job failed; trying again every 30 s");
                        failing = Some(error);
                    }
                }
            }
            if let Err(error) = self.tick_rollups().await {
                tracing::warn!(%error, "rebuilding the rollups failed; reports read the events meanwhile");
            }
            tokio::time::sleep(POLL).await;
        }
    }
}
