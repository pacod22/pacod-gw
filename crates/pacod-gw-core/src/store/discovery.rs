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
    /// `new` (listed, not in the repository), `gone` (in the repository, no longer listed) or
    /// `update` (in the repository, with facts that differ from what the provider reports).
    pub kind: String,
    /// For `gone` and `update`: the repository model's object id.
    pub model_id: Option<String>,
    pub details: serde_json::Value,
    /// `open`, `accepted`, `dismissed` or `resolved`.
    pub status: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub decided_by: Option<String>,
    pub decided_at: Option<DateTime<Utc>>,
    /// Who found it: `provider` (the provider's own model list), `broker` (pacod-broker),
    /// `catalog` (pacod-aggregator's catalogue), `file` (the models file) or `scraper` (a page
    /// an earlier release read).
    pub source: String,
}

const COLUMNS: &str = "id, provider_id, upstream_id, kind, model_id, details, status, first_seen, last_seen, \
                       decided_by, decided_at, source";

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
        source: r.try_get("source")?,
    })
}

impl Store {
    /// The database's clock. A run compares it with the times the database writes, so that a
    /// difference between the gateway's clock and the database's resolves nothing by mistake.
    pub async fn discovery_clock(&self) -> Result<DateTime<Utc>, StoreError> {
        Ok(sqlx::query_scalar::<_, DateTime<Utc>>("SELECT now()").fetch_one(self.pool()).await?)
    }

    /// Records that a run found a difference. An open one is seen again; a resolved or accepted
    /// one opens again. A dismissed one stays dismissed, unless it is an update that reports a
    /// fact it did not report when it was dismissed: the provider has changed something since.
    /// Fewer differences (a hand edit settled some) do not open it again.
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
                 last_seen = now(), model_id = $5, source = 'provider',
                 details = CASE WHEN discovery_proposals.details ? 'broker_resource_id'
                                THEN $6 || jsonb_build_object('broker_resource_id',
                                                              discovery_proposals.details -> 'broker_resource_id')
                                ELSE $6 END,
                 status = CASE WHEN discovery_proposals.status = 'dismissed'
                                    AND (discovery_proposals.kind <> 'update'
                                         OR NOT EXISTS (
                                             SELECT 1 FROM jsonb_each(COALESCE($6 -> 'reported', '{}'::jsonb)) AS n
                                             WHERE discovery_proposals.details -> 'reported' -> n.key
                                                   IS DISTINCT FROM n.value))
                               THEN 'dismissed' ELSE 'open' END",
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

    /// Records what pacod-broker says. A proposal the provider's own list made stays the
    /// provider's: the broker adds its resource id to the details and changes nothing else.
    pub async fn broker_proposal_seen(
        &self,
        id: &str,
        provider_id: &str,
        upstream_id: &str,
        kind: &str,
        model_id: Option<&str>,
        details: &serde_json::Value,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO discovery_proposals (id, provider_id, upstream_id, kind, model_id, details, source)
             VALUES ($1, $2, $3, $4, $5, $6, 'broker')
             ON CONFLICT (provider_id, upstream_id, kind) DO UPDATE SET
                 details = CASE WHEN discovery_proposals.source = 'provider'
                                THEN discovery_proposals.details
                                     || jsonb_build_object('broker_resource_id', $6 -> 'broker_resource_id')
                                ELSE $6 END,
                 source = CASE WHEN discovery_proposals.source IN ('scraper', 'catalog', 'file') THEN 'broker'
                               ELSE discovery_proposals.source END,
                 last_seen = CASE WHEN discovery_proposals.source = 'provider'
                                  THEN discovery_proposals.last_seen ELSE now() END,
                 model_id = COALESCE(discovery_proposals.model_id, $5),
                 status = CASE WHEN discovery_proposals.source = 'provider' THEN discovery_proposals.status
                               WHEN discovery_proposals.status IN ('dismissed', 'accepted') THEN discovery_proposals.status
                               ELSE 'open' END",
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

    /// Resolves the broker's open proposals that its latest run no longer found.
    pub async fn broker_proposals_resolve_unseen(&self, since: DateTime<Utc>) -> Result<u64, StoreError> {
        Ok(sqlx::query(
            "UPDATE discovery_proposals SET status = 'resolved'
             WHERE source = 'broker' AND status = 'open' AND last_seen < $1",
        )
        .bind(since)
        .execute(self.pool())
        .await?
        .rows_affected())
    }

    /// Marks open proposals of a provider as resolved when the latest run no longer found them.
    pub async fn proposals_resolve_unseen(&self, provider_id: &str, since: DateTime<Utc>) -> Result<u64, StoreError> {
        Ok(sqlx::query(
            "UPDATE discovery_proposals SET status = 'resolved'
             WHERE provider_id = $1 AND status = 'open' AND last_seen < $2 AND source = 'provider'",
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
