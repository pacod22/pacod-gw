//! Files and message batches created at a provider through the gateway (concept section 8.3),
//! with the workspace that owns each.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use futures_util::future::BoxFuture;
use schemars::JsonSchema;
use serde::Serialize;
use sqlx::Row;

use super::{Store, StoreError};

/// A file or batch at a provider and who owns it.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ProviderObject {
    pub id: String,
    pub provider_id: String,
    /// The provider API the object belongs to: `anthropic` or `openai`.
    pub api: String,
    /// `file` or `batch`.
    pub kind: String,
    pub organisation_id: String,
    pub workspace_id: String,
    pub key_id: String,
    /// Whose credential created it: `shared`, `organisation` or `workspace`.
    pub credential_scope: String,
    /// Files: `active` or `deleted`. Batches: `in_progress`, `canceling`, `ended`, then
    /// `settled` once accounted, or `deleted`.
    pub status: String,
    /// The provider's object as last seen.
    pub object: serde_json::Value,
    #[serde(skip)]
    pub detail: serde_json::Value,
    /// Batches: the accounted cost, in `currency` (the reporting currency).
    pub cost: Option<f64>,
    pub currency: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub settled_at: Option<DateTime<Utc>>,
}

const COLUMNS: &str = "id, provider_id, api, kind, organisation_id, workspace_id, key_id, credential_scope, status, \
     object, detail, cost, currency, created_at, updated_at, settled_at";

fn object_of(r: &sqlx::postgres::PgRow) -> Result<ProviderObject, sqlx::Error> {
    Ok(ProviderObject {
        id: r.try_get("id")?,
        provider_id: r.try_get("provider_id")?,
        api: r.try_get("api")?,
        kind: r.try_get("kind")?,
        organisation_id: r.try_get("organisation_id")?,
        workspace_id: r.try_get("workspace_id")?,
        key_id: r.try_get("key_id")?,
        credential_scope: r.try_get("credential_scope")?,
        status: r.try_get("status")?,
        object: r.try_get("object")?,
        detail: r.try_get("detail")?,
        cost: r.try_get("cost")?,
        currency: r.try_get("currency")?,
        created_at: r.try_get("created_at")?,
        updated_at: r.try_get("updated_at")?,
        settled_at: r.try_get("settled_at")?,
    })
}

impl Store {
    /// Records an object the provider created. A second record of the same id (a retried
    /// request) keeps the first owner.
    pub async fn object_insert(&self, o: &ProviderObject) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO provider_objects (id, provider_id, kind, organisation_id, workspace_id, key_id, \
                 credential_scope, status, object, detail, currency, api)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12) ON CONFLICT (id) DO NOTHING",
        )
        .bind(&o.id)
        .bind(&o.provider_id)
        .bind(&o.kind)
        .bind(&o.organisation_id)
        .bind(&o.workspace_id)
        .bind(&o.key_id)
        .bind(&o.credential_scope)
        .bind(&o.status)
        .bind(&o.object)
        .bind(&o.detail)
        .bind(&o.currency)
        .bind(&o.api)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn object(&self, id: &str) -> Result<Option<ProviderObject>, StoreError> {
        let row = sqlx::query(&format!("SELECT {COLUMNS} FROM provider_objects WHERE id = $1"))
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        Ok(row.as_ref().map(object_of).transpose()?)
    }

    /// The workspaces that own the given ids, of the objects the gateway knows and that are not
    /// deleted.
    pub async fn object_owners(&self, ids: &[String]) -> Result<HashMap<String, String>, StoreError> {
        let rows =
            sqlx::query("SELECT id, workspace_id FROM provider_objects WHERE id = ANY($1) AND status <> 'deleted'")
                .bind(ids)
                .fetch_all(self.pool())
                .await?;
        rows.iter()
            .map(|r| Ok((r.try_get("id")?, r.try_get("workspace_id")?)))
            .collect::<Result<_, sqlx::Error>>()
            .map_err(Into::into)
    }

    /// A workspace's objects of one kind, of one provider API or of every one, deleted ones
    /// only with `with_deleted`, newest first: `limit` of them after (older than) or before
    /// (newer than) the object `after_id` or `before_id` names.
    #[allow(clippy::too_many_arguments)]
    pub async fn objects_of(
        &self,
        workspace_id: &str,
        api: Option<&str>,
        kind: &str,
        limit: i64,
        after_id: Option<&str>,
        before_id: Option<&str>,
        with_deleted: bool,
    ) -> Result<Vec<ProviderObject>, StoreError> {
        let (condition, order) = match (after_id, before_id) {
            (Some(_), _) => (
                "AND (created_at, id) < (SELECT created_at, id FROM provider_objects WHERE id = $4)",
                "created_at DESC, id DESC",
            ),
            (None, Some(_)) => (
                "AND (created_at, id) > (SELECT created_at, id FROM provider_objects WHERE id = $4)",
                "created_at ASC, id ASC",
            ),
            (None, None) => ("AND $4::text IS NULL", "created_at DESC, id DESC"),
        };
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM provider_objects
             WHERE workspace_id = $1 AND kind = $2 AND ($5 OR status <> 'deleted') AND ($6::text IS NULL OR api = $6)
               {condition}
             ORDER BY {order} LIMIT $3"
        ))
        .bind(workspace_id)
        .bind(kind)
        .bind(limit)
        .bind(after_id.or(before_id))
        .bind(with_deleted)
        .bind(api)
        .fetch_all(self.pool())
        .await?;
        let mut out: Vec<ProviderObject> = rows.iter().map(object_of).collect::<Result<_, _>>()?;
        if before_id.is_some() && after_id.is_none() {
            out.reverse();
        }
        Ok(out)
    }

    /// Records what the provider says about an object now.
    pub async fn object_update(&self, id: &str, status: &str, object: &serde_json::Value) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE provider_objects SET status = $2, object = $3, updated_at = now(), polled_at = now()
             WHERE id = $1 AND status <> 'settled' AND status <> 'deleted'",
        )
        .bind(id)
        .bind(status)
        .bind(object)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn object_deleted(&self, id: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE provider_objects SET status = 'deleted', updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Batches not yet settled whose provider was last asked longest ago, at most `limit`, and
    /// none asked within `min_seconds`.
    pub async fn batches_to_poll(&self, limit: i64, min_seconds: i64) -> Result<Vec<ProviderObject>, StoreError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM provider_objects
             WHERE kind = 'batch' AND status NOT IN ('settled', 'deleted')
               AND (polled_at IS NULL OR polled_at < now() - make_interval(secs => $2))
             ORDER BY polled_at NULLS FIRST LIMIT $1"
        ))
        .bind(limit)
        .bind(min_seconds as f64)
        .fetch_all(self.pool())
        .await?;
        Ok(rows.iter().map(object_of).collect::<Result<_, _>>()?)
    }

    /// Marks a batch as accounted. Returns false when another instance did it first.
    pub async fn batch_settled(&self, id: &str, cost: f64, currency: &str) -> Result<bool, StoreError> {
        let done = sqlx::query(
            "UPDATE provider_objects SET status = 'settled', cost = $2, currency = $3, settled_at = now(), updated_at = now()
             WHERE id = $1 AND status <> 'settled' AND status <> 'deleted'",
        )
        .bind(id)
        .bind(cost)
        .bind(currency)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(done == 1)
    }
}

/// The record of files and batches, behind which store holds it: PostgreSQL in the full
/// gateway, a file of its own in the gateway without a database (`pacod-gw-lite`). The
/// methods are [`Store`]'s, which says what each does.
pub trait ObjectStore: Send + Sync {
    fn object_insert<'a>(&'a self, o: &'a ProviderObject) -> BoxFuture<'a, Result<(), StoreError>>;
    fn object<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<ProviderObject>, StoreError>>;
    fn object_owners<'a>(&'a self, ids: &'a [String]) -> BoxFuture<'a, Result<HashMap<String, String>, StoreError>>;
    #[allow(clippy::too_many_arguments)]
    fn objects_of<'a>(
        &'a self,
        workspace_id: &'a str,
        api: Option<&'a str>,
        kind: &'a str,
        limit: i64,
        after_id: Option<&'a str>,
        before_id: Option<&'a str>,
        with_deleted: bool,
    ) -> BoxFuture<'a, Result<Vec<ProviderObject>, StoreError>>;
    fn object_update<'a>(
        &'a self,
        id: &'a str,
        status: &'a str,
        object: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<(), StoreError>>;
    fn object_deleted<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), StoreError>>;
    fn batches_to_poll(&self, limit: i64, min_seconds: i64) -> BoxFuture<'_, Result<Vec<ProviderObject>, StoreError>>;
    fn batch_settled<'a>(
        &'a self,
        id: &'a str,
        cost: f64,
        currency: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>>;
}

impl ObjectStore for Store {
    fn object_insert<'a>(&'a self, o: &'a ProviderObject) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(Store::object_insert(self, o))
    }

    fn object<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<Option<ProviderObject>, StoreError>> {
        Box::pin(Store::object(self, id))
    }

    fn object_owners<'a>(&'a self, ids: &'a [String]) -> BoxFuture<'a, Result<HashMap<String, String>, StoreError>> {
        Box::pin(Store::object_owners(self, ids))
    }

    fn objects_of<'a>(
        &'a self,
        workspace_id: &'a str,
        api: Option<&'a str>,
        kind: &'a str,
        limit: i64,
        after_id: Option<&'a str>,
        before_id: Option<&'a str>,
        with_deleted: bool,
    ) -> BoxFuture<'a, Result<Vec<ProviderObject>, StoreError>> {
        Box::pin(Store::objects_of(self, workspace_id, api, kind, limit, after_id, before_id, with_deleted))
    }

    fn object_update<'a>(
        &'a self,
        id: &'a str,
        status: &'a str,
        object: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(Store::object_update(self, id, status, object))
    }

    fn object_deleted<'a>(&'a self, id: &'a str) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(Store::object_deleted(self, id))
    }

    fn batches_to_poll(&self, limit: i64, min_seconds: i64) -> BoxFuture<'_, Result<Vec<ProviderObject>, StoreError>> {
        Box::pin(Store::batches_to_poll(self, limit, min_seconds))
    }

    fn batch_settled<'a>(
        &'a self,
        id: &'a str,
        cost: f64,
        currency: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(Store::batch_settled(self, id, cost, currency))
    }
}
