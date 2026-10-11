//! Proposals of model discovery (concept section 6.4), outside the configuration: they do
//! not change what the request path does until an admin accepts one.

use chrono::{DateTime, Utc};
use schemars::JsonSchema;
use serde::Serialize;
use sqlx::Row;

use super::{Store, StoreError};

/// One difference between a provider's list and the repository.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Proposal {
    pub id: String,
    pub provider_id: String,
    pub upstream_id: String,
    /// `new` (listed, not in the repository) or `gone` (in the repository, no longer listed).
    pub kind: String,
    /// For `gone`: the repository model's object id.
    pub model_id: Option<String>,
    pub details: serde_json::Value,
    /// `open`, `accepted`, `dismissed` or `resolved`.
    pub status: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str =
    "id, provider_id, upstream_id, kind, model_id, details, status, first_seen, last_seen, decided_by, decided_at";

fn from_row(r: &sqlx::postgres::PgRow) -> Result<Proposal, sqlx::Error> {
    Ok(Proposal {
        id: r.try_get("id")?,
        provider_id: r.try_get("provider_id")?,
        upstream_id: r.try_get("upstream_id")?,
        kind: r.try_get("kind")?,
        model_id: r.try_get("model_id")?,
        details: r.try_get("details")?,
        status: r.try_get("status")?,
        first_seen: r.try_get("first_seen")?,
        last_seen: r.try_get("last_seen")?,
        decided_by: r.try_get("decided_by")?,
        decided_at: r.try_get("decided_at")?,
    })
}

impl Store {
    /// The database's clock. A run compares it with the times the database writes, so that a
    /// difference between the gateway's clock and the database's resolves nothing by mistake.
    pub async fn discovery_clock(&self) -> Result<DateTime<Utc>, StoreError> {
        Ok(sqlx::query_scalar::<_, DateTime<Utc>>("SELECT now()").fetch_one(self.pool()).await?)
    }

    /// Records that a run found a difference. An open one is seen again; a dismissed one stays
    /// dismissed; a resolved or accepted one opens again.
    pub async fn proposal_seen(
        &self,
        id: &str,
        provider_id: &str,
        upstream_id: &str,
        kind: &str,
        model_id: Option<&str>,
        details: &serde_json::Value,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO discovery_proposals (id, provider_id, upstream_id, kind, model_id, details)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (provider_id, upstream_id, kind) DO UPDATE SET
                 last_seen = now(), details = $6, model_id = $5,
                 status = CASE WHEN discovery_proposals.status = 'dismissed' THEN 'dismissed' ELSE 'open' END",
        )
        .bind(id)
        .bind(provider_id)
        .bind(upstream_id)
        .bind(kind)
        .bind(model_id)
        .bind(details)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Marks open proposals of a provider as resolved when the latest run no longer found them.
    pub async fn proposals_resolve_unseen(&self, provider_id: &str, since: DateTime<Utc>) -> Result<u64, StoreError> {
        Ok(sqlx::query(
            "UPDATE discovery_proposals SET status = 'resolved'
             WHERE provider_id = $1 AND status = 'open' AND last_seen < $2",
        )
        .bind(provider_id)
        .bind(since)
        .execute(self.pool())
        .await?
        .rows_affected())
    }

    pub async fn proposals(
        &self,
        status: Option<&str>,
        provider_id: Option<&str>,
    ) -> Result<Vec<Proposal>, StoreError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM discovery_proposals
             WHERE ($1::text IS NULL OR status = $1) AND ($2::text IS NULL OR provider_id = $2)
             ORDER BY last_seen DESC, upstream_id LIMIT 1000"
        ))
        .bind(status)
        .bind(provider_id)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(from_row).collect::<Result<Vec<_>, _>>()?)
    }

    pub async fn proposal(&self, id: &str) -> Result<Option<Proposal>, StoreError> {
        let row = sqlx::query(&format!("SELECT {COLUMNS} FROM discovery_proposals WHERE id = $1"))
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        Ok(row.as_ref().map(from_row).transpose()?)
    }

    /// Records an admin's decision on an open proposal. False when it is not open.
    pub async fn proposal_decide(&self, id: &str, status: &str, by: &str) -> Result<bool, StoreError> {
        Ok(sqlx::query(
            "UPDATE discovery_proposals SET status = $2, decided_by = $3, decided_at = now()
             WHERE id = $1 AND status = 'open'",
        )
        .bind(id)
        .bind(status)
        .bind(by)
        .execute(self.pool())
        .await?
        .rows_affected()
            == 1)
    }
}
