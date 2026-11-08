//! What probing tool servers saw (`connector_probe_runs`, `connector_probes`), outside the
//! configuration.

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::Row;

use super::{Store, StoreError};

/// How the last probe of a server went.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeRun {
    pub owner_id: String,
    pub probed_at: DateTime<Utc>,
    pub version: String,
    pub ok: bool,
    pub problem: String,
    pub tools: i32,
}

/// One tool a probe saw.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbedItem {
    pub item: String,
    pub definition: Value,
    pub hash: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
}

impl Store {
    /// Records a probe that worked: each tool it saw, in one statement, and the run.
    pub async fn probe_record(
        &self,
        owner_id: &str,
        version: &str,
        items: &[(String, Value, String)],
        at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool().begin().await?;
        if !items.is_empty() {
            let batch: Vec<Value> = items
                .iter()
                .map(|(item, definition, hash)| json!({ "item": item, "definition": definition, "hash": hash }))
                .collect();
            sqlx::query(
                "INSERT INTO connector_probes (owner_id, item, definition, hash, first_seen, last_seen)
                 SELECT $1, r.item, r.definition, r.hash, $3, $3
                 FROM jsonb_to_recordset($2) AS r(item text, definition jsonb, hash text)
                 ON CONFLICT (owner_id, item) DO UPDATE
                     SET definition = EXCLUDED.definition, hash = EXCLUDED.hash, last_seen = EXCLUDED.last_seen",
            )
            .bind(owner_id)
            .bind(Value::Array(batch))
            .bind(at)
            .execute(&mut *tx)
            .await?;
        }
        sqlx::query(
            "INSERT INTO connector_probe_runs (owner_id, probed_at, version, ok, problem, tools)
             VALUES ($1, $2, $3, true, '', $4)
             ON CONFLICT (owner_id) DO UPDATE
                 SET probed_at = $2, version = $3, ok = true, problem = '', tools = $4",
        )
        .bind(owner_id)
        .bind(at)
        .bind(version)
        .bind(items.len() as i32)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Records a probe that failed. What earlier probes saw stays.
    pub async fn probe_failed(&self, owner_id: &str, problem: &str, at: DateTime<Utc>) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO connector_probe_runs (owner_id, probed_at, ok, problem)
             VALUES ($1, $2, false, $3)
             ON CONFLICT (owner_id) DO UPDATE SET probed_at = $2, ok = false, problem = $3",
        )
        .bind(owner_id)
        .bind(at)
        .bind(problem)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The last run of every probed server.
    pub async fn probe_runs(&self) -> Result<Vec<ProbeRun>, StoreError> {
        let rows = sqlx::query("SELECT owner_id, probed_at, version, ok, problem, tools FROM connector_probe_runs")
            .fetch_all(self.pool())
            .await?;
        Ok(rows.iter().map(run_of).collect())
    }

    /// The last run of one server and every tool its probes saw, by name.
    pub async fn probes_of(&self, owner_id: &str) -> Result<(Option<ProbeRun>, Vec<ProbedItem>), StoreError> {
        let run = sqlx::query(
            "SELECT owner_id, probed_at, version, ok, problem, tools FROM connector_probe_runs WHERE owner_id = $1",
        )
        .bind(owner_id)
        .fetch_optional(self.pool())
        .await?;
        let rows = sqlx::query(
            "SELECT item, definition, hash, first_seen, last_seen FROM connector_probes
             WHERE owner_id = $1 ORDER BY item",
        )
        .bind(owner_id)
        .fetch_all(self.pool())
        .await?;
        let items = rows
            .iter()
            .map(|r| ProbedItem {
                item: r.get("item"),
                definition: r.get("definition"),
                hash: r.get("hash"),
                first_seen: r.get("first_seen"),
                last_seen: r.get("last_seen"),
            })
            .collect();
        Ok((run.as_ref().map(run_of), items))
    }

    /// Forgets what probes of a server saw, when the server is deleted.
    pub async fn probes_forget(&self, owner_id: &str) -> Result<(), StoreError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM connector_probes WHERE owner_id = $1").bind(owner_id).execute(&mut *tx).await?;
        sqlx::query("DELETE FROM connector_probe_runs WHERE owner_id = $1").bind(owner_id).execute(&mut *tx).await?;
        tx.commit().await?;
        Ok(())
    }
}

fn run_of(r: &sqlx::postgres::PgRow) -> ProbeRun {
    ProbeRun {
        owner_id: r.get("owner_id"),
        probed_at: r.get("probed_at"),
        version: r.get("version"),
        ok: r.get("ok"),
        problem: r.get("problem"),
        tools: r.get("tools"),
    }
}
