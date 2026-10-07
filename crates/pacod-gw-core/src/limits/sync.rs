//! Keeps the in-memory counters and PostgreSQL in step (concept section 12.4).
//!
//! About once a second each instance adds what it has counted to the stored totals and reads
//! back the totals other instances have changed. Counters therefore survive restarts, and
//! several instances enforce a limit together with a lag of about a second.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::Row as _;
use tokio::sync::watch;

use super::{CellKey, LimitEngine};
use crate::store::Store;

/// How often counters are synchronised.
pub const SYNC_INTERVAL: Duration = Duration::from_secs(1);
/// Windows that ended this long ago are dropped from memory.
const KEEP_AFTER_END_SECONDS: i64 = 120;
/// How far each read of changed counters reaches back before the previous one started.
/// `now()` is the start of the reading transaction, so a write that commits while it runs has
/// an earlier time stamp than the next read asks for. Reading a row twice is harmless.
const REFRESH_OVERLAP: chrono::TimeDelta = chrono::TimeDelta::seconds(2);

/// The stored totals of the given counters. Counters without a row are absent.
pub async fn load_rows(store: &Store, keys: &[CellKey]) -> Result<HashMap<CellKey, u64>, sqlx::Error> {
    if keys.is_empty() {
        return Ok(HashMap::new());
    }
    let limit_ids: Vec<&str> = keys.iter().map(|k| k.limit_id.as_str()).collect();
    let subjects: Vec<&str> = keys.iter().map(|k| k.subject.as_str()).collect();
    let starts: Vec<i64> = keys.iter().map(|k| k.window_start).collect();
    let rows = sqlx::query(
        "SELECT DISTINCT c.limit_id, c.subject, c.window_start, c.used
         FROM limit_counters c
         JOIN UNNEST($1::text[], $2::text[], $3::bigint[]) AS w(limit_id, subject, window_start)
           USING (limit_id, subject, window_start)",
    )
    .bind(&limit_ids)
    .bind(&subjects)
    .bind(&starts)
    .fetch_all(store.pool())
    .await?;
    rows.iter()
        .map(|r| {
            let key = CellKey {
                limit_id: r.try_get("limit_id")?,
                subject: r.try_get("subject")?,
                window_start: r.try_get("window_start")?,
            };
            let used: i64 = r.try_get("used")?;
            Ok((key, used.max(0) as u64))
        })
        .collect()
}

impl LimitEngine {
    /// Adds this instance's increments to the stored totals and takes the new totals as base.
    pub async fn flush(&self, store: &Store) -> Result<usize, sqlx::Error> {
        // Take what is pending. If the write fails, it is put back.
        let taken: Vec<(CellKey, u64)> = {
            let cells = self.cells.read();
            cells
                .iter()
                .filter_map(|(k, c)| {
                    let n = c.pending.swap(0, Ordering::Relaxed);
                    (n > 0).then(|| (k.clone(), n))
                })
                .collect()
        };
        if taken.is_empty() {
            return Ok(0);
        }
        let limit_ids: Vec<&str> = taken.iter().map(|(k, _)| k.limit_id.as_str()).collect();
        let subjects: Vec<&str> = taken.iter().map(|(k, _)| k.subject.as_str()).collect();
        let starts: Vec<i64> = taken.iter().map(|(k, _)| k.window_start).collect();
        let amounts: Vec<i64> = taken.iter().map(|(_, n)| i64::try_from(*n).unwrap_or(i64::MAX)).collect();

        let result = sqlx::query(
            "INSERT INTO limit_counters (limit_id, subject, window_start, used)
             SELECT * FROM UNNEST($1::text[], $2::text[], $3::bigint[], $4::bigint[])
             ON CONFLICT (limit_id, subject, window_start)
             DO UPDATE SET used = limit_counters.used + EXCLUDED.used, updated_at = now()
             RETURNING limit_id, subject, window_start, used",
        )
        .bind(&limit_ids)
        .bind(&subjects)
        .bind(&starts)
        .bind(&amounts)
        .fetch_all(store.pool())
        .await;

        match result {
            Ok(rows) => {
                let cells = self.cells.read();
                for r in &rows {
                    let key = CellKey {
                        limit_id: r.try_get("limit_id")?,
                        subject: r.try_get("subject")?,
                        window_start: r.try_get("window_start")?,
                    };
                    let used: i64 = r.try_get("used")?;
                    if let Some(c) = cells.get(&key) {
                        c.base.store(used.max(0) as u64, Ordering::Relaxed);
                        c.loaded.store(true, Ordering::Relaxed);
                    }
                }
                Ok(taken.len())
            }
            Err(e) => {
                let cells = self.cells.read();
                for (k, n) in &taken {
                    if let Some(c) = cells.get(k) {
                        c.pending.fetch_add(*n, Ordering::Relaxed);
                    }
                }
                Err(e)
            }
        }
    }

    /// Reads the stored totals of counters this instance has just started using, and of
    /// counters other instances changed since `since`. Returns the database time to pass as
    /// `since` next time.
    pub async fn refresh(&self, store: &Store, since: Option<DateTime<Utc>>) -> Result<DateTime<Utc>, sqlx::Error> {
        let db_now: DateTime<Utc> = sqlx::query("SELECT now()").fetch_one(store.pool()).await?.try_get(0)?;

        let wanted: Vec<CellKey> = std::mem::take(&mut *self.to_load.lock());
        if !wanted.is_empty() {
            match load_rows(store, &wanted).await {
                Ok(stored) => {
                    // A counter without a row is a genuine zero: it is loaded too.
                    let cells = self.cells.read();
                    for key in &wanted {
                        if let Some(c) = cells.get(key) {
                            if let Some(used) = stored.get(key) {
                                c.base.store(*used, Ordering::Relaxed);
                            }
                            c.loaded.store(true, Ordering::Relaxed);
                        }
                    }
                }
                Err(e) => {
                    // Ask again next time.
                    self.to_load.lock().extend(wanted);
                    return Err(e);
                }
            }
        }

        if let Some(since) = since {
            let rows =
                sqlx::query("SELECT limit_id, subject, window_start, used FROM limit_counters WHERE updated_at >= $1")
                    .bind(since)
                    .fetch_all(store.pool())
                    .await?;
            self.apply(&rows)?;
        }
        Ok(db_now)
    }

    fn apply(&self, rows: &[sqlx::postgres::PgRow]) -> Result<(), sqlx::Error> {
        let cells = self.cells.read();
        for r in rows {
            let key = CellKey {
                limit_id: r.try_get("limit_id")?,
                subject: r.try_get("subject")?,
                window_start: r.try_get("window_start")?,
            };
            let used: i64 = r.try_get("used")?;
            if let Some(c) = cells.get(&key) {
                c.base.store(used.max(0) as u64, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    /// Drops counters of windows that have ended and have nothing left to write.
    pub fn prune(&self, now: DateTime<Utc>) -> usize {
        let cutoff = now.timestamp() - KEEP_AFTER_END_SECONDS;
        let mut cells = self.cells.write();
        let before = cells.len();
        cells.retain(|_, c| c.window_end > cutoff || c.pending.load(Ordering::Relaxed) > 0);
        before - cells.len()
    }

    /// Sets a subject's stored counters for one limit back to zero: the reset operation. The
    /// rows are kept, so that every other instance reads the new total at its next round.
    pub async fn reset_stored(&self, store: &Store, limit_id: &str, subject: &str) -> Result<u64, sqlx::Error> {
        self.reset(limit_id, subject);
        Ok(sqlx::query("UPDATE limit_counters SET used = 0, updated_at = now() WHERE limit_id = $1 AND subject = $2")
            .bind(limit_id)
            .bind(subject)
            .execute(store.pool())
            .await?
            .rows_affected())
    }

    /// Runs the synchronisation until `shutdown` fires, then writes what is left.
    pub async fn run(self: Arc<Self>, store: Store, mut shutdown: watch::Receiver<bool>) {
        let mut tick = tokio::time::interval(SYNC_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut since = None;
        let mut rounds = 0u32;
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = shutdown.changed() => break,
            }
            if let Err(e) = self.flush(&store).await {
                tracing::warn!(error = %e, "writing limit counters failed; they stay in memory and are retried");
            }
            match self.refresh(&store, since).await {
                Ok(t) => since = Some(t - REFRESH_OVERLAP),
                Err(e) => tracing::debug!(error = %e, "reading limit counters failed"),
            }
            self.rounds.send_modify(|n| *n += 1);
            rounds += 1;
            if rounds.is_multiple_of(60) {
                self.prune(Utc::now());
                // Stored counters of long-past windows are of no use to anyone.
                let _ = sqlx::query(
                    "DELETE FROM limit_counters WHERE window_start > 0 AND window_start < extract(epoch from now() - interval '62 days')",
                )
                .execute(store.pool())
                .await;
            }
        }
        let _ = self.flush(&store).await;
    }
}
