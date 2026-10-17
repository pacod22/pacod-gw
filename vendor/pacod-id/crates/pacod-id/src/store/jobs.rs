//! Leases for background jobs: one instance at a time runs a job.

use super::{Store, StoreError};

impl Store {
    /// Takes or renews the lease of `name` for `holder`. False when another holder has it.
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

    /// Gives a lease back, so another instance can take it at once (on shutdown).
    pub async fn job_release(&self, name: &str, holder: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE jobs SET holder = '', lease_until = NULL WHERE name = $1 AND holder = $2")
            .bind(name)
            .bind(holder)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn job_ran(&self, name: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE jobs SET last_run_at = now() WHERE name = $1").bind(name).execute(self.pool()).await?;
        Ok(())
    }
}
