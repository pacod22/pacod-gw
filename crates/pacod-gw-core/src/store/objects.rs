//! Files and message batches created at a provider through the gateway (concept section 8.3),
//! with the workspace that owns each.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use futures_util::future::BoxFuture;
use schemars::JsonSchema;
use serde::Serialize;
use sqlx::Row;

use super::conversations::{Conversation, ConversationItem, Items, Page};
use super::responses::{Chain, StoredResponse, Whose};
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

/// A batch item's request, kept for its audit record from the call that brought it until its
/// batch is settled (concept section 14.6): as the record keeps it, with its size before that.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingItem {
    pub custom_id: String,
    pub workspace_id: String,
    pub end_user: String,
    /// The item as the caller sent it.
    pub received: Vec<u8>,
    pub received_bytes: i64,
    pub received_truncated: bool,
    /// The item as it went to the provider.
    pub sent: Vec<u8>,
    pub sent_bytes: i64,
    pub sent_truncated: bool,
    pub resolution: serde_json::Value,
    /// The end user's records were erased: the item is not recorded.
    pub erased: bool,
    pub expires_at: DateTime<Utc>,
}

/// The most rows, and bytes of bodies, one statement writes: a row holds two bodies of up to the
/// audit size limit, and a statement must stay well under PostgreSQL's message limit.
const ITEM_ROWS: usize = 500;
const ITEM_BYTES: usize = 32 << 20;

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
    /// none asked within `min_seconds` unless the last answer said it has ended (a caller's
    /// call about it saw that): such a batch is settled without waiting.
    pub async fn batches_to_poll(&self, limit: i64, min_seconds: i64) -> Result<Vec<ProviderObject>, StoreError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM provider_objects
             WHERE kind = 'batch' AND status NOT IN ('settled', 'deleted')
               AND (polled_at IS NULL OR polled_at < now() - make_interval(secs => $2) OR status IN ('ended', 'completed', 'failed', 'expired', 'cancelled'))
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

impl Store {
    /// Keeps batch items' requests under `key`, replacing what was kept for the same item.
    pub async fn items_keep(&self, key: &str, items: &[PendingItem]) -> Result<(), StoreError> {
        let mut start = 0;
        while start < items.len() {
            let mut end = start;
            let mut bytes = 0;
            while end < items.len() && end - start < ITEM_ROWS && (end == start || bytes < ITEM_BYTES) {
                bytes += items[end].received.len() + items[end].sent.len();
                end += 1;
            }
            let chunk = &items[start..end];
            let col = |f: fn(&PendingItem) -> String| chunk.iter().map(f).collect::<Vec<_>>();
            sqlx::query(
                "INSERT INTO audit_batch_items (items_key, custom_id, workspace_id, end_user, received, received_bytes, \
                     received_truncated, sent, sent_bytes, sent_truncated, resolution, expires_at)
                 SELECT $1, * FROM UNNEST($2::text[], $3::text[], $4::text[], $5::bytea[], $6::int8[], $7::bool[], \
                     $8::bytea[], $9::int8[], $10::bool[], $11::jsonb[], $12::timestamptz[])
                 ON CONFLICT (items_key, custom_id) DO UPDATE SET workspace_id = EXCLUDED.workspace_id,
                     end_user = EXCLUDED.end_user, received = EXCLUDED.received,
                     received_bytes = EXCLUDED.received_bytes, received_truncated = EXCLUDED.received_truncated,
                     sent = EXCLUDED.sent, sent_bytes = EXCLUDED.sent_bytes, sent_truncated = EXCLUDED.sent_truncated,
                     resolution = EXCLUDED.resolution, erased = false, expires_at = EXCLUDED.expires_at",
            )
            .bind(key)
            .bind(col(|i| i.custom_id.clone()))
            .bind(col(|i| i.workspace_id.clone()))
            .bind(col(|i| i.end_user.clone()))
            .bind(chunk.iter().map(|i| i.received.clone()).collect::<Vec<_>>())
            .bind(chunk.iter().map(|i| i.received_bytes).collect::<Vec<_>>())
            .bind(chunk.iter().map(|i| i.received_truncated).collect::<Vec<_>>())
            .bind(chunk.iter().map(|i| i.sent.clone()).collect::<Vec<_>>())
            .bind(chunk.iter().map(|i| i.sent_bytes).collect::<Vec<_>>())
            .bind(chunk.iter().map(|i| i.sent_truncated).collect::<Vec<_>>())
            .bind(chunk.iter().map(|i| i.resolution.clone()).collect::<Vec<_>>())
            .bind(chunk.iter().map(|i| i.expires_at).collect::<Vec<_>>())
            .execute(self.pool())
            .await?;
            start = end;
        }
        Ok(())
    }

    /// Of the items `custom_ids` names, those kept under `key`, in the order of their ids, as
    /// many as fit in `max_bytes` of bodies (at least one). The caller asks again for the rest.
    pub async fn items_of(
        &self,
        key: &str,
        custom_ids: &[String],
        max_bytes: i64,
    ) -> Result<HashMap<String, PendingItem>, StoreError> {
        let rows = sqlx::query(
            "SELECT custom_id, workspace_id, end_user, received, received_bytes, received_truncated, sent, sent_bytes,
                    sent_truncated, resolution, erased, expires_at
             FROM (SELECT *, sum(octet_length(received) + octet_length(sent)) OVER (ORDER BY custom_id) AS running
                   FROM audit_batch_items WHERE items_key = $1 AND custom_id = ANY($2)) t
             WHERE running - (octet_length(received) + octet_length(sent)) < $3",
        )
        .bind(key)
        .bind(custom_ids)
        .bind(max_bytes)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                let item = PendingItem {
                    custom_id: r.try_get("custom_id")?,
                    workspace_id: r.try_get("workspace_id")?,
                    end_user: r.try_get("end_user")?,
                    received: r.try_get("received")?,
                    received_bytes: r.try_get("received_bytes")?,
                    received_truncated: r.try_get("received_truncated")?,
                    sent: r.try_get("sent")?,
                    sent_bytes: r.try_get("sent_bytes")?,
                    sent_truncated: r.try_get("sent_truncated")?,
                    resolution: r.try_get("resolution")?,
                    erased: r.try_get("erased")?,
                    expires_at: r.try_get("expires_at")?,
                };
                Ok((item.custom_id.clone(), item))
            })
            .collect::<Result<_, sqlx::Error>>()
            .map_err(Into::into)
    }

    /// Forgets what was kept under `key`.
    pub async fn items_drop(&self, key: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM audit_batch_items WHERE items_key = $1").bind(key).execute(self.pool()).await?;
        Ok(())
    }

    /// Erases the requests kept for one end user of a workspace. The rows stay, emptied, so that
    /// their items are not recorded when their batches settle. Returns how many.
    pub async fn items_erase(&self, workspace_id: &str, end_user: &str) -> Result<u64, StoreError> {
        let done = sqlx::query(
            "UPDATE audit_batch_items SET received = '', sent = '', resolution = '{}', erased = true
             WHERE workspace_id = $1 AND end_user = $2 AND NOT erased",
        )
        .bind(workspace_id)
        .bind(end_user)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(done)
    }

    /// Forgets what was kept past its time: a batch that never settled, an input file never
    /// deleted. Returns how many.
    pub async fn items_expire(&self) -> Result<u64, StoreError> {
        let done = sqlx::query("DELETE FROM audit_batch_items WHERE expires_at < now()")
            .execute(self.pool())
            .await?
            .rows_affected();
        Ok(done)
    }
}

/// The statuses of a batch that has ended at its provider: Anthropic's, then OpenAI's.
pub const ENDED: [&str; 5] = ["ended", "completed", "failed", "expired", "cancelled"];

/// The record of files and batches, and of stored responses, behind which store holds it:
/// PostgreSQL in the full gateway, a file of its own in the gateway without a database
/// (`pacod-gw-lite`), which keeps no responses. The methods are [`Store`]'s, which says what
/// each does.
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

    /// Keeps batch items' requests for their audit records. False where this store keeps none
    /// (the minimal gateway's, which records no batch items).
    fn items_keep<'a>(&'a self, _key: &'a str, _items: &'a [PendingItem]) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async { Ok(false) })
    }
    fn items_of<'a>(
        &'a self,
        _key: &'a str,
        _custom_ids: &'a [String],
        _max_bytes: i64,
    ) -> BoxFuture<'a, Result<HashMap<String, PendingItem>, StoreError>> {
        Box::pin(async { Ok(HashMap::new()) })
    }
    fn items_drop<'a>(&'a self, _key: &'a str) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(async { Ok(()) })
    }
    fn items_erase<'a>(&'a self, _workspace_id: &'a str, _end_user: &'a str) -> BoxFuture<'a, Result<u64, StoreError>> {
        Box::pin(async { Ok(0) })
    }
    fn items_expire(&self) -> BoxFuture<'_, Result<u64, StoreError>> {
        Box::pin(async { Ok(0) })
    }

    /// Whether this store keeps responses of the Responses API ([`Store::response_keep`] and
    /// the methods after it). Where it keeps none (the minimal gateway's), `/v1/responses`
    /// stays stateless and the methods below answer as for a store that holds nothing.
    fn keeps_responses(&self) -> bool {
        false
    }
    /// False where this store keeps none.
    fn response_keep<'a>(&'a self, _r: &'a StoredResponse) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async { Ok(false) })
    }
    fn response<'a>(
        &'a self,
        _workspace_id: &'a str,
        _id: &'a str,
    ) -> BoxFuture<'a, Result<Option<StoredResponse>, StoreError>> {
        Box::pin(async { Ok(None) })
    }
    fn response_chain<'a>(
        &'a self,
        _workspace_id: &'a str,
        id: &'a str,
        _max_bytes: i64,
    ) -> BoxFuture<'a, Result<Chain, StoreError>> {
        Box::pin(async move { Ok(Chain::Missing(id.to_owned())) })
    }
    fn response_delete<'a>(&'a self, _workspace_id: &'a str, _id: &'a str) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async { Ok(false) })
    }
    fn responses_erase<'a>(
        &'a self,
        _workspace_id: &'a str,
        _whose: Whose<'a>,
    ) -> BoxFuture<'a, Result<u64, StoreError>> {
        Box::pin(async { Ok(0) })
    }
    fn responses_expire(&self) -> BoxFuture<'_, Result<u64, StoreError>> {
        Box::pin(async { Ok(0) })
    }

    /// Conversations ([`Store::conversation_create`] and the methods after it): kept where
    /// responses are. False where this store keeps none.
    fn conversation_create<'a>(
        &'a self,
        _c: &'a Conversation,
        _items: &'a [ConversationItem],
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async { Ok(false) })
    }
    fn conversation<'a>(
        &'a self,
        _workspace_id: &'a str,
        _id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Conversation>, StoreError>> {
        Box::pin(async { Ok(None) })
    }
    fn conversation_update<'a>(
        &'a self,
        _workspace_id: &'a str,
        _id: &'a str,
        _metadata: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Option<Conversation>, StoreError>> {
        Box::pin(async { Ok(None) })
    }
    fn conversation_delete<'a>(
        &'a self,
        _workspace_id: &'a str,
        _id: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async { Ok(false) })
    }
    fn conversation_add<'a>(
        &'a self,
        _workspace_id: &'a str,
        _id: &'a str,
        _items: &'a [ConversationItem],
        _expires_at: DateTime<Utc>,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async { Ok(false) })
    }
    fn conversation_items<'a>(
        &'a self,
        _workspace_id: &'a str,
        _id: &'a str,
        _after: Option<&'a str>,
        _limit: i64,
        _ascending: bool,
    ) -> BoxFuture<'a, Result<Option<Page>, StoreError>> {
        Box::pin(async { Ok(None) })
    }
    fn conversation_item<'a>(
        &'a self,
        _workspace_id: &'a str,
        _id: &'a str,
        _item_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ConversationItem>, StoreError>> {
        Box::pin(async { Ok(None) })
    }
    fn conversation_item_delete<'a>(
        &'a self,
        _workspace_id: &'a str,
        _id: &'a str,
        _item_id: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async { Ok(false) })
    }
    fn conversation_replay<'a>(
        &'a self,
        _workspace_id: &'a str,
        _id: &'a str,
        _max_bytes: i64,
    ) -> BoxFuture<'a, Result<Items, StoreError>> {
        Box::pin(async { Ok(Items::Missing) })
    }
    fn conversation_turn<'a>(
        &'a self,
        _response: Option<&'a StoredResponse>,
        _workspace_id: &'a str,
        _id: &'a str,
        _items: &'a [ConversationItem],
        _expires_at: DateTime<Utc>,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async { Ok(false) })
    }
    fn conversations_erase<'a>(
        &'a self,
        _workspace_id: &'a str,
        _whose: Whose<'a>,
    ) -> BoxFuture<'a, Result<(u64, u64), StoreError>> {
        Box::pin(async { Ok((0, 0)) })
    }
    fn conversations_expire(&self) -> BoxFuture<'_, Result<u64, StoreError>> {
        Box::pin(async { Ok(0) })
    }
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

    fn items_keep<'a>(&'a self, key: &'a str, items: &'a [PendingItem]) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move { Store::items_keep(self, key, items).await.map(|()| true) })
    }

    fn items_of<'a>(
        &'a self,
        key: &'a str,
        custom_ids: &'a [String],
        max_bytes: i64,
    ) -> BoxFuture<'a, Result<HashMap<String, PendingItem>, StoreError>> {
        Box::pin(Store::items_of(self, key, custom_ids, max_bytes))
    }

    fn items_drop<'a>(&'a self, key: &'a str) -> BoxFuture<'a, Result<(), StoreError>> {
        Box::pin(Store::items_drop(self, key))
    }

    fn items_erase<'a>(&'a self, workspace_id: &'a str, end_user: &'a str) -> BoxFuture<'a, Result<u64, StoreError>> {
        Box::pin(Store::items_erase(self, workspace_id, end_user))
    }

    fn items_expire(&self) -> BoxFuture<'_, Result<u64, StoreError>> {
        Box::pin(Store::items_expire(self))
    }

    fn keeps_responses(&self) -> bool {
        true
    }

    fn response_keep<'a>(&'a self, r: &'a StoredResponse) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move { Store::response_keep(self, r).await.map(|()| true) })
    }

    fn response<'a>(
        &'a self,
        workspace_id: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<StoredResponse>, StoreError>> {
        Box::pin(Store::response(self, workspace_id, id))
    }

    fn response_chain<'a>(
        &'a self,
        workspace_id: &'a str,
        id: &'a str,
        max_bytes: i64,
    ) -> BoxFuture<'a, Result<Chain, StoreError>> {
        Box::pin(Store::response_chain(self, workspace_id, id, max_bytes))
    }

    fn response_delete<'a>(&'a self, workspace_id: &'a str, id: &'a str) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(Store::response_delete(self, workspace_id, id))
    }

    fn responses_erase<'a>(
        &'a self,
        workspace_id: &'a str,
        whose: Whose<'a>,
    ) -> BoxFuture<'a, Result<u64, StoreError>> {
        Box::pin(Store::responses_erase(self, workspace_id, whose))
    }

    fn responses_expire(&self) -> BoxFuture<'_, Result<u64, StoreError>> {
        Box::pin(Store::responses_expire(self))
    }

    fn conversation_create<'a>(
        &'a self,
        c: &'a Conversation,
        items: &'a [ConversationItem],
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(async move { Store::conversation_create(self, c, items).await.map(|()| true) })
    }

    fn conversation<'a>(
        &'a self,
        workspace_id: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Result<Option<Conversation>, StoreError>> {
        Box::pin(Store::conversation(self, workspace_id, id))
    }

    fn conversation_update<'a>(
        &'a self,
        workspace_id: &'a str,
        id: &'a str,
        metadata: &'a serde_json::Value,
    ) -> BoxFuture<'a, Result<Option<Conversation>, StoreError>> {
        Box::pin(Store::conversation_update(self, workspace_id, id, metadata))
    }

    fn conversation_delete<'a>(
        &'a self,
        workspace_id: &'a str,
        id: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(Store::conversation_delete(self, workspace_id, id))
    }

    fn conversation_add<'a>(
        &'a self,
        workspace_id: &'a str,
        id: &'a str,
        items: &'a [ConversationItem],
        expires_at: DateTime<Utc>,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(Store::conversation_add(self, workspace_id, id, items, expires_at))
    }

    fn conversation_items<'a>(
        &'a self,
        workspace_id: &'a str,
        id: &'a str,
        after: Option<&'a str>,
        limit: i64,
        ascending: bool,
    ) -> BoxFuture<'a, Result<Option<Page>, StoreError>> {
        Box::pin(Store::conversation_items(self, workspace_id, id, after, limit, ascending))
    }

    fn conversation_item<'a>(
        &'a self,
        workspace_id: &'a str,
        id: &'a str,
        item_id: &'a str,
    ) -> BoxFuture<'a, Result<Option<ConversationItem>, StoreError>> {
        Box::pin(Store::conversation_item(self, workspace_id, id, item_id))
    }

    fn conversation_item_delete<'a>(
        &'a self,
        workspace_id: &'a str,
        id: &'a str,
        item_id: &'a str,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(Store::conversation_item_delete(self, workspace_id, id, item_id))
    }

    fn conversation_replay<'a>(
        &'a self,
        workspace_id: &'a str,
        id: &'a str,
        max_bytes: i64,
    ) -> BoxFuture<'a, Result<Items, StoreError>> {
        Box::pin(Store::conversation_replay(self, workspace_id, id, max_bytes))
    }

    fn conversation_turn<'a>(
        &'a self,
        response: Option<&'a StoredResponse>,
        workspace_id: &'a str,
        id: &'a str,
        items: &'a [ConversationItem],
        expires_at: DateTime<Utc>,
    ) -> BoxFuture<'a, Result<bool, StoreError>> {
        Box::pin(Store::conversation_turn(self, response, workspace_id, id, items, expires_at))
    }

    fn conversations_erase<'a>(
        &'a self,
        workspace_id: &'a str,
        whose: Whose<'a>,
    ) -> BoxFuture<'a, Result<(u64, u64), StoreError>> {
        Box::pin(Store::conversations_erase(self, workspace_id, whose))
    }

    fn conversations_expire(&self) -> BoxFuture<'_, Result<u64, StoreError>> {
        Box::pin(Store::conversations_expire(self))
    }
}
