//! The identity outbox (migration 0014): organisation and workspace changes on their way to
//! pacod-id, in order.

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::Row as _;

use super::{Store, StoreError};

/// A change waiting for pacod-id.
#[derive(Debug, Clone)]
pub struct IdpRow {
    pub id: i64,
    pub op: String,
    pub target: String,
    pub payload: Value,
    pub attempts: i32,
    pub next_attempt_at: DateTime<Utc>,
}

/// Queues a change. A parked row for the same target is replaced: the newer change supersedes
/// it, and the table keeps at most one refused row per target.
pub(super) async fn enqueue(
    conn: &mut sqlx::PgConnection,
    op: &str,
    target: &str,
    payload: &Value,
) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM idp_outbox WHERE target = $1 AND parked_at IS NOT NULL")
        .bind(target)
        .execute(&mut *conn)
        .await?;
    sqlx::query("INSERT INTO idp_outbox (op, target, payload) VALUES ($1, $2, $3)")
        .bind(op)
        .bind(target)
        .bind(payload)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The advisory lock key of the outbox's deliverer ("idpoutbx" as bytes).
const DELIVERER: i64 = 0x6964_706f_7574_6278;

impl Store {
    /// Queues a change outside a configuration change (the sweep), if configuration object
    /// `id` still exists: the sweep reads a snapshot that may lag. The row lock orders this
    /// with a deletion of the object, whose own row then comes after this one.
    pub async fn idp_enqueue_existing(
        &self,
        id: &str,
        op: &str,
        target: &str,
        payload: &Value,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool.begin().await?;
        let exists: Option<i32> = sqlx::query_scalar("SELECT 1 FROM config_objects WHERE id = $1 FOR SHARE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
        if exists.is_none() {
            return Ok(false);
        }
        enqueue(&mut tx, op, target, payload).await?;
        tx.commit().await?;
        Ok(true)
    }

    /// The deliverer's turn: a transaction holding the outbox's advisory lock, or `None` while
    /// another deliverer holds it. Rows are read and finished within it, so order holds.
    pub async fn idp_turn(&self) -> Result<Option<sqlx::Transaction<'static, sqlx::Postgres>>, StoreError> {
        let mut tx = self.pool.begin().await?;
        let got: bool =
            sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)").bind(DELIVERER).fetch_one(&mut *tx).await?;
        Ok(got.then_some(tx))
    }

    /// The rows not parked, oldest first.
    pub async fn idp_pending(
        tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
        limit: i64,
    ) -> Result<Vec<IdpRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, op, target, payload, attempts, next_attempt_at FROM idp_outbox
             WHERE parked_at IS NULL ORDER BY id LIMIT $1",
        )
        .bind(limit)
        .fetch_all(&mut **tx)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(IdpRow {
                    id: r.try_get("id")?,
                    op: r.try_get("op")?,
                    target: r.try_get("target")?,
                    payload: r.try_get("payload")?,
                    attempts: r.try_get("attempts")?,
                    next_attempt_at: r.try_get("next_attempt_at")?,
                })
            })
            .collect::<Result<_, sqlx::Error>>()
            .map_err(Into::into)
    }

    pub async fn idp_done(tx: &mut sqlx::Transaction<'static, sqlx::Postgres>, id: i64) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM idp_outbox WHERE id = $1").bind(id).execute(&mut **tx).await?;
        Ok(())
    }

    /// A failed attempt: tried again after a backoff (2^attempts seconds, at most 5 minutes),
    /// or parked for good.
    pub async fn idp_failed(
        tx: &mut sqlx::Transaction<'static, sqlx::Postgres>,
        id: i64,
        error: &str,
        park: bool,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE idp_outbox SET attempts = attempts + 1, last_error = $2,
               next_attempt_at = now() + least(interval '5 minutes', interval '1 second' * power(2, attempts)),
               parked_at = CASE WHEN $3 THEN now() END
             WHERE id = $1",
        )
        .bind(id)
        .bind(error)
        .bind(park)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// The targets with a row waiting, so that the sweep does not queue them twice.
    pub async fn idp_targets(&self) -> Result<std::collections::HashSet<String>, StoreError> {
        let rows: Vec<String> = sqlx::query_scalar("SELECT DISTINCT target FROM idp_outbox WHERE parked_at IS NULL")
            .fetch_all(&self.pool)
            .await?;
        Ok(rows.into_iter().collect())
    }

    /// Why pacod-id refused a target's last parked row, when one is parked: the scope then stays
    /// unknown there until an operator acts.
    pub async fn idp_parked(&self, target: &str) -> Result<Option<String>, StoreError> {
        let r: Option<Option<String>> = sqlx::query_scalar(
            "SELECT last_error FROM idp_outbox WHERE target = $1 AND parked_at IS NOT NULL ORDER BY id DESC LIMIT 1",
        )
        .bind(target)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(Option::unwrap_or_default))
    }

    /// Rows waiting and rows parked, for the status page and the log.
    pub async fn idp_counts(&self) -> Result<(i64, i64), StoreError> {
        let r = sqlx::query(
            "SELECT count(*) FILTER (WHERE parked_at IS NULL), count(*) FILTER (WHERE parked_at IS NOT NULL) FROM idp_outbox",
        )
        .fetch_one(&self.pool)
        .await?;
        Ok((r.try_get(0)?, r.try_get(1)?))
    }
}
