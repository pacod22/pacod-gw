//! The instance registry: the core processes that run, the configuration version each has
//! applied, and the live figures each publishes for the central console.

use chrono::{DateTime, Utc};
use sqlx::Row as _;
use sqlx::postgres::PgRow;

use super::{InstanceRow, Store, StoreError};

/// What a heartbeat records.
pub struct Heartbeat<'a> {
    pub id: &'a str,
    pub role: &'a str,
    pub hostname: &'a str,
    pub software_version: &'a str,
    pub applied_version: i64,
    pub started_at: DateTime<Utc>,
    pub boot_id: &'a str,
    pub home_id: Option<&'a str>,
    /// An instance counts as running while its last heartbeat is younger than this.
    pub alive_seconds: i64,
}

/// An instance with the live figures it last published.
#[derive(Debug, Clone)]
pub struct InstanceRecord {
    pub row: InstanceRow,
    pub live: Option<serde_json::Value>,
    pub live_at: Option<DateTime<Utc>>,
}

const ROW_COLUMNS: &str =
    "id, role, hostname, software_version, applied_version, started_at, heartbeat_at, conflict_at";

fn instance_row(r: &PgRow) -> Result<InstanceRow, sqlx::Error> {
    Ok(InstanceRow {
        id: r.try_get("id")?,
        role: r.try_get("role")?,
        hostname: r.try_get("hostname")?,
        software_version: r.try_get("software_version")?,
        applied_version: r.try_get("applied_version")?,
        started_at: r.try_get("started_at")?,
        heartbeat_at: r.try_get("heartbeat_at")?,
        conflict_at: r.try_get("conflict_at")?,
    })
}

impl Store {
    /// Records that this process runs. Returns false, and notes the conflict on the row, when
    /// another process that is still running holds the id. A row is taken over when it belongs
    /// to this process, to an earlier process with the same spool directory (a restart), to an
    /// instance of the previous version, or to a process that stopped heart-beating.
    pub async fn instance_heartbeat(&self, h: &Heartbeat<'_>) -> Result<bool, StoreError> {
        let taken = sqlx::query(
            "INSERT INTO instances AS i
                 (id, role, hostname, software_version, applied_version, started_at, heartbeat_at, boot_id, home_id)
             VALUES ($1, $2, $3, $4, $5, $6, now(), $7, $8)
             ON CONFLICT (id) DO UPDATE SET
                 role = EXCLUDED.role,
                 hostname = EXCLUDED.hostname,
                 software_version = EXCLUDED.software_version,
                 applied_version = EXCLUDED.applied_version,
                 started_at = EXCLUDED.started_at,
                 heartbeat_at = now(),
                 boot_id = EXCLUDED.boot_id,
                 home_id = EXCLUDED.home_id,
                 live = CASE WHEN i.boot_id IS DISTINCT FROM EXCLUDED.boot_id THEN NULL ELSE i.live END,
                 live_at = CASE WHEN i.boot_id IS DISTINCT FROM EXCLUDED.boot_id THEN NULL ELSE i.live_at END,
                 -- A new process has applied nothing of pacod-id's feed yet (and in local mode never will).
                 authz_seq = CASE WHEN i.boot_id IS DISTINCT FROM EXCLUDED.boot_id THEN NULL ELSE i.authz_seq END
             WHERE i.boot_id = EXCLUDED.boot_id
                OR i.boot_id IS NULL
                OR i.home_id = EXCLUDED.home_id
                OR i.heartbeat_at < now() - make_interval(secs => $9)
             RETURNING 1",
        )
        .bind(h.id)
        .bind(h.role)
        .bind(h.hostname)
        .bind(h.software_version)
        .bind(h.applied_version)
        .bind(h.started_at)
        .bind(h.boot_id)
        .bind(h.home_id)
        .bind(h.alive_seconds as f64)
        .fetch_optional(&self.pool)
        .await?;
        if taken.is_some() {
            return Ok(true);
        }
        sqlx::query("UPDATE instances SET conflict_at = now() WHERE id = $1").bind(h.id).execute(&self.pool).await?;
        Ok(false)
    }

    /// Records how far this instance's copy of pacod-id's feed has come (OIDC mode).
    pub async fn instance_authz_seq(&self, id: &str, seq: i64) -> Result<(), StoreError> {
        sqlx::query("UPDATE instances SET authz_seq = $2 WHERE id = $1").bind(id).bind(seq).execute(&self.pool).await?;
        Ok(())
    }

    /// Of the running management instances in OIDC mode, how many have followed pacod-id's
    /// feed to `seq`, and how many there are.
    pub async fn instances_authz_applied(&self, alive_seconds: i64, seq: i64) -> Result<(i64, i64), StoreError> {
        let r = sqlx::query(
            "SELECT count(*) FILTER (WHERE authz_seq >= $2), count(*) FROM instances
             WHERE heartbeat_at > now() - make_interval(secs => $1) AND authz_seq IS NOT NULL",
        )
        .bind(alive_seconds as f64)
        .bind(seq)
        .fetch_one(&self.pool)
        .await?;
        Ok((r.try_get(0)?, r.try_get(1)?))
    }

    /// The host and last heartbeat of whoever holds an instance id.
    pub async fn instance_holder(&self, id: &str) -> Result<Option<(String, DateTime<Utc>)>, StoreError> {
        let row = sqlx::query("SELECT hostname, heartbeat_at FROM instances WHERE id = $1")
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(row.map(|r| r.try_get(0).and_then(|h| Ok((h, r.try_get(1)?)))).transpose()?)
    }

    /// Instances seen within the last `alive_seconds`.
    pub async fn instances(&self, alive_seconds: i64) -> Result<Vec<InstanceRow>, StoreError> {
        let rows = sqlx::query(&format!(
            "SELECT {ROW_COLUMNS} FROM instances WHERE heartbeat_at > now() - make_interval(secs => $1) ORDER BY id"
        ))
        .bind(alive_seconds as f64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(instance_row).collect::<Result<Vec<_>, _>>()?)
    }

    /// Instances seen within the last `alive_seconds`, with their live figures.
    pub async fn instance_records(&self, alive_seconds: i64) -> Result<Vec<InstanceRecord>, StoreError> {
        let rows = sqlx::query(&format!(
            "SELECT {ROW_COLUMNS}, live, live_at FROM instances
             WHERE heartbeat_at > now() - make_interval(secs => $1) ORDER BY id"
        ))
        .bind(alive_seconds as f64)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows
            .iter()
            .map(|r| {
                Ok(InstanceRecord { row: instance_row(r)?, live: r.try_get("live")?, live_at: r.try_get("live_at")? })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?)
    }

    /// Instances that stopped heart-beating without signing off: seen within the last
    /// `keep_hours`, but not within the last `alive_seconds`.
    pub async fn instances_stopped(&self, alive_seconds: i64, keep_hours: i64) -> Result<Vec<InstanceRow>, StoreError> {
        let rows = sqlx::query(&format!(
            "SELECT {ROW_COLUMNS} FROM instances
             WHERE heartbeat_at <= now() - make_interval(secs => $1)
               AND heartbeat_at > now() - make_interval(hours => $2)
             ORDER BY heartbeat_at DESC"
        ))
        .bind(alive_seconds as f64)
        .bind(keep_hours as i32)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(instance_row).collect::<Result<Vec<_>, _>>()?)
    }

    /// How many running instances serve the data plane.
    pub async fn data_plane_instance_count(&self, alive_seconds: i64) -> Result<usize, StoreError> {
        let n: i64 = sqlx::query(
            "SELECT count(*) FROM instances
             WHERE role IN ('gateway', 'all') AND heartbeat_at > now() - make_interval(secs => $1)",
        )
        .bind(alive_seconds as f64)
        .fetch_one(&self.pool)
        .await?
        .try_get(0)?;
        Ok(usize::try_from(n).unwrap_or(0))
    }

    /// Stores this process's live figures on its row.
    pub async fn instance_publish(&self, id: &str, boot_id: &str, live: &serde_json::Value) -> Result<(), StoreError> {
        sqlx::query("UPDATE instances SET live = $3, live_at = now() WHERE id = $1 AND boot_id = $2")
            .bind(id)
            .bind(boot_id)
            .bind(live)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Removes this process's row when it stops. A row another process has taken over stays.
    pub async fn instance_remove(&self, id: &str, boot_id: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM instances WHERE id = $1 AND boot_id = $2")
            .bind(id)
            .bind(boot_id)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Forgets instances that have not been seen for `hours`.
    pub async fn instances_prune(&self, hours: i64) -> Result<u64, StoreError> {
        Ok(sqlx::query("DELETE FROM instances WHERE heartbeat_at < now() - make_interval(hours => $1)")
            .bind(hours as i32)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }
}
