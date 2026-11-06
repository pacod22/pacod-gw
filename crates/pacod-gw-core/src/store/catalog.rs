//! What each source of model data last said about each model (`catalog_seen`), outside the
//! configuration, and the proposals of model discovery the sync makes.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::Row;

use super::{Store, StoreError};

/// One entry a source listed, as a run records it.
#[derive(Debug, Clone)]
pub struct Seen {
    pub vendor: String,
    /// Normalised (lower case).
    pub api_id: String,
    /// The catalogue's entry.
    pub entry: Value,
    /// The gateway's model it matched.
    pub model_id: Option<String>,
}

impl Store {
    /// What `source` last listed, by vendor and normalised API id.
    pub async fn catalog_seen(&self, source: &str) -> Result<HashMap<(String, String), Value>, StoreError> {
        let rows = sqlx::query("SELECT vendor, api_id, entry FROM catalog_seen WHERE source = $1")
            .bind(source)
            .fetch_all(self.pool())
            .await?;
        Ok(rows.iter().map(|r| ((r.get("vendor"), r.get("api_id")), r.get("entry"))).collect())
    }

    /// Records what a run read, in one statement: an entry listed again replaces what was there.
    /// `rows` holds each vendor and id once.
    pub async fn catalog_seen_record(&self, source: &str, rows: &[Seen]) -> Result<(), StoreError> {
        if rows.is_empty() {
            return Ok(());
        }
        let batch: Vec<Value> = rows
            .iter()
            .map(|r| json!({ "vendor": r.vendor, "api_id": r.api_id, "entry": r.entry, "model_id": r.model_id }))
            .collect();
        sqlx::query(
            "INSERT INTO catalog_seen (source, vendor, api_id, entry, model_id, seen_at)
             SELECT $1, r.vendor, r.api_id, r.entry, r.model_id, now()
             FROM jsonb_to_recordset($2) AS r(vendor text, api_id text, entry jsonb, model_id text)
             ON CONFLICT (source, vendor, api_id) DO UPDATE
                 SET entry = EXCLUDED.entry, model_id = EXCLUDED.model_id, seen_at = EXCLUDED.seen_at",
        )
        .bind(source)
        .bind(Value::Array(batch))
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Opens or refreshes the sync's proposal of a model a catalogue lists that the repository
    /// lacks (`source` is `catalog` or `file`). As with the price tracker's, the provider's own
    /// list stays the authority: a row of another source is never taken over, and a proposal an
    /// administrator dismissed or accepted is not opened again.
    pub async fn catalog_proposal_seen(
        &self,
        id: &str,
        provider_id: &str,
        upstream_id: &str,
        details: &Value,
        source: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO discovery_proposals (id, provider_id, upstream_id, kind, details, source)
             VALUES ($1, $2, $3, 'new', $4, $5)
             ON CONFLICT (provider_id, upstream_id, kind) DO UPDATE SET
                 details = CASE WHEN discovery_proposals.source = $5 THEN $4
                                ELSE discovery_proposals.details END,
                 last_seen = CASE WHEN discovery_proposals.source = $5 THEN now()
                                  ELSE discovery_proposals.last_seen END,
                 status = CASE WHEN discovery_proposals.source <> $5 THEN discovery_proposals.status
                               WHEN discovery_proposals.status IN ('dismissed', 'accepted') THEN discovery_proposals.status
                               ELSE 'open' END",
        )
        .bind(id)
        .bind(provider_id)
        .bind(upstream_id)
        .bind(details)
        .bind(source)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Resolves the open proposals of `source` for a provider that its latest reading no longer
    /// listed.
    pub async fn catalog_proposals_resolve_unseen(
        &self,
        provider_id: &str,
        source: &str,
        since: DateTime<Utc>,
    ) -> Result<u64, StoreError> {
        Ok(sqlx::query(
            "UPDATE discovery_proposals SET status = 'resolved'
             WHERE provider_id = $1 AND source = $2 AND status = 'open' AND last_seen < $3",
        )
        .bind(provider_id)
        .bind(source)
        .bind(since)
        .execute(self.pool())
        .await?
        .rows_affected())
    }
}
