//! Leases and runs of the batch jobs the analytics service executes (concept section 13.3).

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::Row as _;

use super::{Store, StoreError};

/// What the core knows about a job.
#[derive(Debug, Clone, Default)]
pub struct JobState {
    pub holder: String,
    pub lease_until: Option<DateTime<Utc>>,
    pub last_started_at: Option<DateTime<Utc>>,
    pub run_requested: bool,
    pub current_run: Option<String>,
}

/// One run of a job.
#[derive(Debug, Clone, Serialize, schemars::JsonSchema)]
pub struct JobRun {
    pub id: String,
    pub job: String,
    /// `schedule`, `manual` or `import`.
    pub trigger: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Absent while the run is in progress.
    pub ok: Option<bool>,
    pub message: String,
    /// Per-model results and counts.
    pub result: serde_json::Value,
}

fn run_of(r: &sqlx::postgres::PgRow) -> Result<JobRun, sqlx::Error> {
    Ok(JobRun {
        id: r.try_get("id")?,
        job: r.try_get("job")?,
        trigger: r.try_get("trigger")?,
        started_at: r.try_get("started_at")?,
        finished_at: r.try_get("finished_at")?,
        ok: r.try_get("ok")?,
        message: r.try_get("message")?,
        result: r.try_get("result")?,
    })
}

impl Store {
    pub async fn job_state(&self, name: &str) -> Result<JobState, StoreError> {
        let row = sqlx::query(
            "SELECT holder, lease_until, last_started_at, run_requested, current_run FROM jobs WHERE name = $1",
        )
        .bind(name)
        .fetch_optional(self.pool())
        .await?;
        Ok(match row {
            None => JobState::default(),
            Some(r) => JobState {
                holder: r.try_get("holder")?,
                lease_until: r.try_get("lease_until")?,
                last_started_at: r.try_get("last_started_at")?,
                run_requested: r.try_get("run_requested")?,
                current_run: r.try_get("current_run")?,
            },
        })
    }

    /// Asks for a run as soon as an analytics instance next takes the lease.
    pub async fn job_request_run(&self, name: &str) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO jobs (name, run_requested) VALUES ($1, true)
             ON CONFLICT (name) DO UPDATE SET run_requested = true",
        )
        .bind(name)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Takes or renews the lease for `holder`. Returns false when another holder has it.
    pub async fn job_lease(&self, name: &str, holder: &str, ttl_seconds: u32) -> Result<bool, StoreError> {
        let taken = sqlx::query(
            "INSERT INTO jobs (name, holder, lease_until) VALUES ($1, $2, now() + make_interval(secs => $3))
             ON CONFLICT (name) DO UPDATE SET holder = $2, lease_until = now() + make_interval(secs => $3)
             WHERE jobs.holder = $2 OR jobs.holder = '' OR jobs.lease_until IS NULL OR jobs.lease_until < now()",
        )
        .bind(name)
        .bind(holder)
        .bind(f64::from(ttl_seconds))
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(taken == 1)
    }

    /// Records the start of a run and clears a pending "run now".
    pub async fn job_start_run(&self, name: &str, run_id: &str, trigger: &str) -> Result<(), StoreError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("INSERT INTO job_runs (id, job, trigger, started_at) VALUES ($1, $2, $3, now())")
            .bind(run_id)
            .bind(name)
            .bind(trigger)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO jobs (name, last_started_at, run_requested, current_run) VALUES ($1, now(), false, $2)
             ON CONFLICT (name) DO UPDATE SET last_started_at = now(), run_requested = false, current_run = $2",
        )
        .bind(name)
        .bind(run_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Records the end of a run. Returns false when the run is unknown or already finished.
    pub async fn job_finish_run(
        &self,
        run_id: &str,
        ok: bool,
        message: &str,
        result: &serde_json::Value,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool().begin().await?;
        let done = sqlx::query(
            "UPDATE job_runs SET finished_at = now(), ok = $2, message = $3, result = $4
             WHERE id = $1 AND finished_at IS NULL",
        )
        .bind(run_id)
        .bind(ok)
        .bind(message)
        .bind(result)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        sqlx::query("UPDATE jobs SET current_run = NULL, holder = '', lease_until = NULL WHERE current_run = $1")
            .bind(run_id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(done == 1)
    }

    pub async fn job_run(&self, run_id: &str) -> Result<Option<JobRun>, StoreError> {
        let row = sqlx::query("SELECT * FROM job_runs WHERE id = $1").bind(run_id).fetch_optional(self.pool()).await?;
        Ok(row.as_ref().map(run_of).transpose()?)
    }

    /// The runs of a job, newest first.
    pub async fn job_runs(&self, name: &str, limit: i64) -> Result<Vec<JobRun>, StoreError> {
        let rows = sqlx::query("SELECT * FROM job_runs WHERE job = $1 ORDER BY started_at DESC LIMIT $2")
            .bind(name)
            .bind(limit)
            .fetch_all(self.pool())
            .await?;
        Ok(rows.iter().map(run_of).collect::<Result<_, _>>()?)
    }
}
