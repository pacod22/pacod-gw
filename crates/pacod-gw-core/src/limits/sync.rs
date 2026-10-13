//! Keeps the in-memory counters and PostgreSQL in step (concept section 12.4).
//!
//! About once a second each instance adds what it has counted to the stored totals and reads
//! back the totals any instance has changed, including counters it has not used itself, so it
//! holds a copy of every counter of a running window (loaded in full at start). Counters
//! therefore survive restarts, and several instances enforce a limit together with a lag of
//! about a second, also for a subject one of them sees for the first time.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::Row as _;
use tokio::sync::watch;

use super::{Cell, CellKey, LimitEngine};
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
        // Take what is pending. It keeps counting as `flushing` until the new total is in
        // `base`; if the write fails, it is put back.
        let taken: Vec<(CellKey, u64, i64)> = {
            let cells = self.cells.read();
            cells
                .iter()
                .filter_map(|(k, c)| {
                    let n = c.pending.swap(0, Ordering::Relaxed);
                    c.flushing.fetch_add(n, Ordering::Relaxed);
                    (n > 0).then(|| (k.clone(), n, c.window_end))
                })
                .collect()
        };
        if taken.is_empty() {
            return Ok(0);
        }
        let limit_ids: Vec<&str> = taken.iter().map(|(k, _, _)| k.limit_id.as_str()).collect();
        let subjects: Vec<&str> = taken.iter().map(|(k, _, _)| k.subject.as_str()).collect();
        let starts: Vec<i64> = taken.iter().map(|(k, _, _)| k.window_start).collect();
        let amounts: Vec<i64> = taken.iter().map(|(_, n, _)| i64::try_from(*n).unwrap_or(i64::MAX)).collect();
        let ends: Vec<i64> = taken.iter().map(|(_, _, end)| *end).collect();

        let result = sqlx::query(
            "INSERT INTO limit_counters (limit_id, subject, window_start, used, window_end)
             SELECT * FROM UNNEST($1::text[], $2::text[], $3::bigint[], $4::bigint[], $5::bigint[])
             ON CONFLICT (limit_id, subject, window_start)
             DO UPDATE SET used = limit_counters.used + EXCLUDED.used, updated_at = now(),
                 window_end = COALESCE(limit_counters.window_end, EXCLUDED.window_end)
             RETURNING limit_id, subject, window_start, used",
        )
        .bind(&limit_ids)
        .bind(&subjects)
        .bind(&starts)
        .bind(&amounts)
        .bind(&ends)
        .fetch_all(store.pool())
        .await;

        match result {
            Ok(rows) => {
                let totals = rows
                    .iter()
                    .map(|r| {
                        let key = CellKey {
                            limit_id: r.try_get("limit_id")?,
                            subject: r.try_get("subject")?,
                            window_start: r.try_get("window_start")?,
                        };
                        Ok((key, r.try_get::<i64, _>("used")?))
                    })
                    .collect::<Result<Vec<_>, sqlx::Error>>();
                let cells = self.cells.read();
                for (key, used) in totals.iter().flatten() {
                    if let Some(c) = cells.get(key) {
                        c.base.store((*used).max(0) as u64, Ordering::Relaxed);
                        c.loaded.store(true, Ordering::Relaxed);
                    }
                }
                // Only now: until here a reader would have missed what was being written.
                for (k, n, _) in &taken {
                    if let Some(c) = cells.get(k) {
                        c.flushing.fetch_sub(*n, Ordering::Relaxed);
                    }
                }
                totals.map(|_| taken.len())
            }
            Err(e) => {
                let cells = self.cells.read();
                for (k, n, _) in &taken {
                    if let Some(c) = cells.get(k) {
                        c.flushing.fetch_sub(*n, Ordering::Relaxed);
                        c.pending.fetch_add(*n, Ordering::Relaxed);
                    }
                }
                Err(e)
            }
        }
    }

    /// Reads every stored counter of a window that has not ended, so that this instance knows
    /// what the others have counted before it serves a call. Returns the database time to
    /// pass to [`refresh`](Self::refresh) as `since`.
    pub async fn load_current(&self, store: &Store) -> Result<DateTime<Utc>, sqlx::Error> {
        let db_now: DateTime<Utc> = sqlx::query("SELECT now()").fetch_one(store.pool()).await?.try_get(0)?;
        let rows = sqlx::query(
            "SELECT limit_id, subject, window_start, used, window_end FROM limit_counters WHERE window_end > $1",
        )
        .bind(db_now.timestamp() - KEEP_AFTER_END_SECONDS)
        .fetch_all(store.pool())
        .await?;
        self.apply(&rows, db_now.timestamp())?;
        Ok(db_now)
    }

    /// Reads the stored totals of counters this instance has just started using, and of
    /// counters any instance changed since `since`, including counters this instance has not
    /// used yet. Returns the database time to pass as `since` next time.
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
            let rows = sqlx::query(
                "SELECT limit_id, subject, window_start, used, window_end FROM limit_counters WHERE updated_at >= $1",
            )
            .bind(since)
            .fetch_all(store.pool())
            .await?;
            self.apply(&rows, db_now.timestamp())?;
        }
        Ok(db_now)
    }

    /// Takes stored totals as the base of this instance's counters. A counter this instance
    /// does not have yet is created when its window is still running, so that its first
    /// call already sees what the other instances counted.
    fn apply(&self, rows: &[sqlx::postgres::PgRow], now: i64) -> Result<(), sqlx::Error> {
        let mut missing = Vec::new();
        {
            let cells = self.cells.read();
            for r in rows {
                let key = CellKey {
                    limit_id: r.try_get("limit_id")?,
                    subject: r.try_get("subject")?,
                    window_start: r.try_get("window_start")?,
                };
                let used = r.try_get::<i64, _>("used")?.max(0) as u64;
                match cells.get(&key) {
                    Some(c) => {
                        c.base.store(used, Ordering::Relaxed);
                        c.loaded.store(true, Ordering::Relaxed);
                    }
                    None => {
                        // Rows written before window ends were stored are read on first use.
                        let end: Option<i64> = r.try_get("window_end")?;
                        if let Some(end) = end.filter(|end| *end > now - KEEP_AFTER_END_SECONDS) {
                            missing.push((key, used, end));
                        }
                    }
                }
            }
        }
        if !missing.is_empty() {
            let mut cells = self.cells.write();
            for (key, used, end) in missing {
                // A call may have created it in the meantime; it keeps what it counted.
                let c = cells.entry(key).or_insert_with(|| Arc::new(Cell { window_end: end, ..Default::default() }));
                c.base.store(used, Ordering::Relaxed);
                c.loaded.store(true, Ordering::Relaxed);
            }
        }
        Ok(())
    }

    /// Drops counters of windows that have ended and have nothing left to write.
    pub fn prune(&self, now: DateTime<Utc>) -> usize {
        let cutoff = now.timestamp() - KEEP_AFTER_END_SECONDS;
        let mut cells = self.cells.write();
        let before = cells.len();
        cells.retain(|_, c| c.window_end > cutoff || c.unwritten() > 0);
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

    /// Runs the synchronisation until `shutdown` fires, then writes what is left. `since` is
    /// the time [`load_current`](Self::load_current) returned, if it ran.
    pub async fn run(self: Arc<Self>, store: Store, since: Option<DateTime<Utc>>, mut shutdown: watch::Receiver<bool>) {
        let mut tick = tokio::time::interval(SYNC_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut since = since.map(|t| t - REFRESH_OVERLAP);
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

    /// One round for an instance without a database (`pacod-gw-lite`): what it counted becomes
    /// its total, and every counter counts as read, since there is nothing stored to wait for.
    pub fn fold_local(&self) {
        let cells = self.cells.read();
        for c in cells.values() {
            // Added before it is taken away: a reader in between sees too much, never too little.
            let n = c.pending.load(Ordering::Relaxed);
            c.base.fetch_add(n, Ordering::Relaxed);
            c.pending.fetch_sub(n, Ordering::Relaxed);
            c.loaded.store(true, Ordering::Relaxed);
        }
        drop(cells);
        self.to_load.lock().clear();
        self.rounds.send_modify(|n| *n += 1);
    }

    /// [`run`](Self::run) without a database: folds the counts every second and drops ended
    /// windows every minute, until `shutdown` fires. Without it the counters of a gateway
    /// without a database would grow without end and the quota endpoint would always wait.
    pub async fn run_local(self: Arc<Self>, mut shutdown: watch::Receiver<bool>) {
        let mut tick = tokio::time::interval(SYNC_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let mut rounds = 0u32;
        loop {
            tokio::select! {
                _ = tick.tick() => {}
                _ = shutdown.changed() => break,
            }
            self.fold_local();
            rounds += 1;
            if rounds.is_multiple_of(60) {
                self.prune(Utc::now());
            }
        }
    }
}
