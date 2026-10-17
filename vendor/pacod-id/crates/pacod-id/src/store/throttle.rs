//! Fixed-window counters with a lock, shared by every instance. Sign-in failures
//! and mail requests are rare enough that a row update per event costs nothing.

use chrono::{DateTime, Utc};
use sqlx::Row;

use super::{Store, StoreError};

/// The state of a counter after a hit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hit {
    /// Events in the current window, this one included.
    pub count: i32,
    /// Set when the limit was reached: until when the key is locked.
    pub locked_until: Option<DateTime<Utc>>,
}

impl Store {
    /// Counts an event for `(bucket, key)` in a window of `window_seconds`. When the count
    /// reaches `limit`, the key is locked for `lock_seconds`.
    pub async fn throttle_hit(
        &self,
        bucket: &str,
        key: &str,
        window_seconds: u32,
        limit: i32,
        lock_seconds: u32,
    ) -> Result<Hit, StoreError> {
        let row = sqlx::query(
            "INSERT INTO throttle (bucket, key, window_start, count) VALUES ($1, $2, now(), 1)
             ON CONFLICT (bucket, key) DO UPDATE SET
               count = CASE WHEN throttle.window_start < now() - make_interval(secs => $3) THEN 1 ELSE throttle.count + 1 END,
               window_start = CASE WHEN throttle.window_start < now() - make_interval(secs => $3) THEN now() ELSE throttle.window_start END
             RETURNING count",
        )
        .bind(bucket)
        .bind(key)
        .bind(f64::from(window_seconds))
        .fetch_one(self.pool())
        .await?;
        let count: i32 = row.try_get("count")?;
        let locked_until = if count >= limit {
            let r = sqlx::query(
                "UPDATE throttle SET locked_until = now() + make_interval(secs => $3)
                 WHERE bucket = $1 AND key = $2 RETURNING locked_until",
            )
            .bind(bucket)
            .bind(key)
            .bind(f64::from(lock_seconds))
            .fetch_one(self.pool())
            .await?;
            r.try_get("locked_until")?
        } else {
            None
        };
        Ok(Hit { count, locked_until })
    }

    /// Takes a slot before the work it guards (a password check): the event is counted first,
    /// so that a burst of parallel requests cannot all pass a check made before any of them is
    /// counted. Over the limit, the key is locked for `lock_seconds` and the answer is false.
    pub async fn throttle_take(
        &self,
        bucket: &str,
        key: &str,
        window_seconds: u32,
        limit: i32,
        lock_seconds: u32,
    ) -> Result<bool, StoreError> {
        let row = sqlx::query(
            "INSERT INTO throttle (bucket, key, window_start, count) VALUES ($1, $2, now(), 1)
             ON CONFLICT (bucket, key) DO UPDATE SET
               count = CASE WHEN throttle.window_start < now() - make_interval(secs => $3) THEN 1 ELSE throttle.count + 1 END,
               window_start = CASE WHEN throttle.window_start < now() - make_interval(secs => $3) THEN now() ELSE throttle.window_start END
             RETURNING count",
        )
        .bind(bucket)
        .bind(key)
        .bind(f64::from(window_seconds))
        .fetch_one(self.pool())
        .await?;
        let count: i32 = row.try_get("count")?;
        if count <= limit {
            return Ok(true);
        }
        sqlx::query(
            "UPDATE throttle SET locked_until = greatest(locked_until, now() + make_interval(secs => $3))
             WHERE bucket = $1 AND key = $2",
        )
        .bind(bucket)
        .bind(key)
        .bind(f64::from(lock_seconds))
        .execute(self.pool())
        .await?;
        Ok(false)
    }

    /// Gives back a slot `throttle_take` took for an attempt that turned out well.
    pub async fn throttle_give_back(&self, bucket: &str, key: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE throttle SET count = greatest(count - 1, 0) WHERE bucket = $1 AND key = $2")
            .bind(bucket)
            .bind(key)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Until when `(bucket, key)` is locked, if it is.
    pub async fn throttle_locked(&self, bucket: &str, key: &str) -> Result<Option<DateTime<Utc>>, StoreError> {
        let row =
            sqlx::query("SELECT locked_until FROM throttle WHERE bucket = $1 AND key = $2 AND locked_until > now()")
                .bind(bucket)
                .bind(key)
                .fetch_optional(self.pool())
                .await?;
        Ok(row.map(|r| r.try_get("locked_until")).transpose()?.flatten())
    }

    /// Forgets a counter (after a successful sign-in).
    pub async fn throttle_clear(&self, bucket: &str, key: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM throttle WHERE bucket = $1 AND key = $2")
            .bind(bucket)
            .bind(key)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Removes counters whose window and lock both lie in the past.
    pub async fn throttle_prune(&self, older_than_seconds: u32) -> Result<u64, StoreError> {
        Ok(sqlx::query(
            "DELETE FROM throttle WHERE window_start < now() - make_interval(secs => $1)
             AND (locked_until IS NULL OR locked_until < now())",
        )
        .bind(f64::from(older_than_seconds))
        .execute(self.pool())
        .await?
        .rows_affected())
    }
}
