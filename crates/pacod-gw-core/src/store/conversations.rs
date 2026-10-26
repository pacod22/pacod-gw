//! Conversations of OpenAI's Responses API (`/v1/conversations`) that the gateway keeps for a
//! workspace: their items in order, which a call that names the conversation is given before
//! its own input, and to which its input and answer are added.

use chrono::{DateTime, Utc};
use sqlx::Row;

use super::responses::{StoredResponse, Whose};
use super::{Store, StoreError};

/// A conversation, without its items.
#[derive(Debug, Clone, PartialEq)]
pub struct Conversation {
    /// `conv_<uuid>`.
    pub id: String,
    pub organisation_id: String,
    pub workspace_id: String,
    pub key_id: String,
    pub end_user: String,
    pub user_id: String,
    /// Up to 16 string pairs, as the caller set them.
    pub metadata: serde_json::Value,
    pub created_at: DateTime<Utc>,
    /// When an item was last added: the retention counts from here.
    pub used_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// An item of a conversation.
#[derive(Debug, Clone, PartialEq)]
pub struct ConversationItem {
    /// The item's own id where it has one, else one the gateway gave it. It is not written
    /// into the item: a provider is given the item as it was sent or answered.
    pub item_id: String,
    pub end_user: String,
    pub user_id: String,
    /// The response whose input or answer it was, if any.
    pub response_id: Option<String>,
    /// The item as JSON.
    pub item: Vec<u8>,
}

/// A conversation's items, as a call that names it needs them.
#[derive(Debug, Clone, PartialEq)]
pub enum Items {
    /// In order, oldest first.
    Found(Vec<Vec<u8>>),
    /// No such conversation in the workspace, or it has expired.
    Missing,
    /// The items hold more than the bytes allowed.
    TooLarge,
}

/// One page of a conversation's items.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub items: Vec<ConversationItem>,
    pub has_more: bool,
}

const COLUMNS: &str =
    "id, organisation_id, workspace_id, key_id, end_user, user_id, metadata, created_at, used_at, expires_at";

/// The most rows one statement of the expiry deletes.
const EXPIRE_ROWS: i64 = 5000;

fn conversation_of(r: &sqlx::postgres::PgRow) -> Result<Conversation, sqlx::Error> {
    Ok(Conversation {
        id: r.try_get("id")?,
        organisation_id: r.try_get("organisation_id")?,
        workspace_id: r.try_get("workspace_id")?,
        key_id: r.try_get("key_id")?,
        end_user: r.try_get("end_user")?,
        user_id: r.try_get("user_id")?,
        metadata: r.try_get("metadata")?,
        created_at: r.try_get("created_at")?,
        used_at: r.try_get("used_at")?,
        expires_at: r.try_get("expires_at")?,
    })
}

fn item_of(r: &sqlx::postgres::PgRow) -> Result<ConversationItem, sqlx::Error> {
    Ok(ConversationItem {
        item_id: r.try_get("item_id")?,
        end_user: r.try_get("end_user")?,
        user_id: r.try_get("user_id")?,
        response_id: r.try_get("response_id")?,
        item: r.try_get("item")?,
    })
}

/// Adds items to a conversation of the workspace and pushes its expiry out, in `tx`. Returns
/// false when there is no such conversation (any more).
async fn append(
    tx: &mut sqlx::PgConnection,
    workspace_id: &str,
    id: &str,
    items: &[ConversationItem],
    expires_at: DateTime<Utc>,
) -> Result<bool, StoreError> {
    let found = sqlx::query(
        "UPDATE conversations SET used_at = now(), expires_at = greatest(expires_at, $3)
         WHERE workspace_id = $1 AND id = $2 AND expires_at > now()",
    )
    .bind(workspace_id)
    .bind(id)
    .bind(expires_at)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if found == 0 {
        return Ok(false);
    }
    if items.is_empty() {
        return Ok(true);
    }
    let col = |f: fn(&ConversationItem) -> String| items.iter().map(f).collect::<Vec<_>>();
    sqlx::query(
        "INSERT INTO conversation_items (conversation_id, workspace_id, item_id, end_user, user_id, response_id, item)
         SELECT $1, $2, t.item_id, t.end_user, t.user_id, t.response_id, t.item
         FROM UNNEST($3::text[], $4::text[], $5::text[], $6::text[], $7::bytea[]) WITH ORDINALITY
             AS t(item_id, end_user, user_id, response_id, item, n)
         ORDER BY t.n",
    )
    .bind(id)
    .bind(workspace_id)
    .bind(col(|i| i.item_id.clone()))
    .bind(col(|i| i.end_user.clone()))
    .bind(col(|i| i.user_id.clone()))
    .bind(items.iter().map(|i| i.response_id.clone()).collect::<Vec<_>>())
    .bind(items.iter().map(|i| i.item.clone()).collect::<Vec<_>>())
    .execute(&mut *tx)
    .await?;
    Ok(true)
}

impl Store {
    /// Creates a conversation with its first items.
    pub async fn conversation_create(&self, c: &Conversation, items: &[ConversationItem]) -> Result<(), StoreError> {
        let mut tx = self.pool().begin().await?;
        sqlx::query(
            "INSERT INTO conversations (id, organisation_id, workspace_id, key_id, end_user, user_id, metadata, \
                 created_at, used_at, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(&c.id)
        .bind(&c.organisation_id)
        .bind(&c.workspace_id)
        .bind(&c.key_id)
        .bind(&c.end_user)
        .bind(&c.user_id)
        .bind(&c.metadata)
        .bind(c.created_at)
        .bind(c.used_at)
        .bind(c.expires_at)
        .execute(&mut *tx)
        .await?;
        append(&mut tx, &c.workspace_id, &c.id, items, c.expires_at).await?;
        tx.commit().await?;
        Ok(())
    }

    /// A workspace's conversation, unless it has expired.
    pub async fn conversation(&self, workspace_id: &str, id: &str) -> Result<Option<Conversation>, StoreError> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM conversations WHERE workspace_id = $1 AND id = $2 AND expires_at > now()"
        ))
        .bind(workspace_id)
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.as_ref().map(conversation_of).transpose()?)
    }

    /// Replaces a conversation's metadata. Returns the conversation, or `None` when there is
    /// none.
    pub async fn conversation_update(
        &self,
        workspace_id: &str,
        id: &str,
        metadata: &serde_json::Value,
    ) -> Result<Option<Conversation>, StoreError> {
        let row = sqlx::query(&format!(
            "UPDATE conversations SET metadata = $3 WHERE workspace_id = $1 AND id = $2 AND expires_at > now()
             RETURNING {COLUMNS}"
        ))
        .bind(workspace_id)
        .bind(id)
        .bind(metadata)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.as_ref().map(conversation_of).transpose()?)
    }

    /// Deletes a conversation with its items. Returns whether there was one.
    pub async fn conversation_delete(&self, workspace_id: &str, id: &str) -> Result<bool, StoreError> {
        let done = sqlx::query("DELETE FROM conversations WHERE workspace_id = $1 AND id = $2")
            .bind(workspace_id)
            .bind(id)
            .execute(self.pool())
            .await?
            .rows_affected();
        Ok(done == 1)
    }

    /// Adds items to a conversation and pushes its expiry out. Returns false when there is no
    /// such conversation.
    pub async fn conversation_add(
        &self,
        workspace_id: &str,
        id: &str,
        items: &[ConversationItem],
        expires_at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool().begin().await?;
        let found = append(&mut tx, workspace_id, id, items, expires_at).await?;
        tx.commit().await?;
        Ok(found)
    }

    /// A page of a conversation's items: `limit` of them after the item `after`, oldest first
    /// or newest first. `None` when there is no such conversation.
    pub async fn conversation_items(
        &self,
        workspace_id: &str,
        id: &str,
        after: Option<&str>,
        limit: i64,
        ascending: bool,
    ) -> Result<Option<Page>, StoreError> {
        if self.conversation(workspace_id, id).await?.is_none() {
            return Ok(None);
        }
        let (cmp, order) = if ascending { (">", "ASC") } else { ("<", "DESC") };
        let rows = sqlx::query(&format!(
            "SELECT item_id, end_user, user_id, response_id, item FROM conversation_items
             WHERE conversation_id = $1 AND workspace_id = $2
               AND ($3::text IS NULL OR seq {cmp} (SELECT seq FROM conversation_items
                    WHERE conversation_id = $1 AND item_id = $3 ORDER BY seq DESC LIMIT 1))
             ORDER BY seq {order} LIMIT $4"
        ))
        .bind(id)
        .bind(workspace_id)
        .bind(after)
        .bind(limit + 1)
        .fetch_all(self.pool())
        .await?;
        let mut items: Vec<ConversationItem> = rows.iter().map(item_of).collect::<Result<_, _>>()?;
        let has_more = items.len() as i64 > limit;
        items.truncate(limit as usize);
        Ok(Some(Page { items, has_more }))
    }

    /// One item of a conversation: the newest of that id.
    pub async fn conversation_item(
        &self,
        workspace_id: &str,
        id: &str,
        item_id: &str,
    ) -> Result<Option<ConversationItem>, StoreError> {
        if self.conversation(workspace_id, id).await?.is_none() {
            return Ok(None);
        }
        let row = sqlx::query(
            "SELECT item_id, end_user, user_id, response_id, item FROM conversation_items
             WHERE conversation_id = $1 AND workspace_id = $2 AND item_id = $3 ORDER BY seq DESC LIMIT 1",
        )
        .bind(id)
        .bind(workspace_id)
        .bind(item_id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.as_ref().map(item_of).transpose()?)
    }

    /// Deletes the items of an id from a conversation. Returns whether there was one.
    pub async fn conversation_item_delete(
        &self,
        workspace_id: &str,
        id: &str,
        item_id: &str,
    ) -> Result<bool, StoreError> {
        let done = sqlx::query(
            "DELETE FROM conversation_items WHERE conversation_id = $1 AND workspace_id = $2 AND item_id = $3",
        )
        .bind(id)
        .bind(workspace_id)
        .bind(item_id)
        .execute(self.pool())
        .await?
        .rows_affected();
        Ok(done > 0)
    }

    /// Every item of a conversation, oldest first, read only when together they hold at most
    /// `max_bytes`.
    pub async fn conversation_replay(&self, workspace_id: &str, id: &str, max_bytes: i64) -> Result<Items, StoreError> {
        let Some(_) = self.conversation(workspace_id, id).await? else { return Ok(Items::Missing) };
        let bytes: Option<i64> = sqlx::query(
            "SELECT sum(octet_length(item))::int8 FROM conversation_items WHERE conversation_id = $1 AND workspace_id = $2",
        )
        .bind(id)
        .bind(workspace_id)
        .fetch_one(self.pool())
        .await?
        .try_get(0)?;
        if bytes.unwrap_or(0) > max_bytes {
            return Ok(Items::TooLarge);
        }
        let rows = sqlx::query(
            "SELECT item FROM conversation_items WHERE conversation_id = $1 AND workspace_id = $2 ORDER BY seq",
        )
        .bind(id)
        .bind(workspace_id)
        .fetch_all(self.pool())
        .await?;
        Ok(Items::Found(rows.iter().map(|r| r.try_get("item")).collect::<Result<_, _>>()?))
    }

    /// Keeps a turn of a conversation in one transaction: the response, where it is kept, and
    /// the turn's input and answer as items of the conversation. Returns false when the
    /// conversation is gone; then nothing is kept.
    pub async fn conversation_turn(
        &self,
        response: Option<&StoredResponse>,
        workspace_id: &str,
        id: &str,
        items: &[ConversationItem],
        expires_at: DateTime<Utc>,
    ) -> Result<bool, StoreError> {
        let mut tx = self.pool().begin().await?;
        if !append(&mut tx, workspace_id, id, items, expires_at).await? {
            return Ok(false);
        }
        if let Some(r) = response {
            super::responses::insert(&mut tx, r).await?;
        }
        tx.commit().await?;
        Ok(true)
    }

    /// Deletes the conversations one user of a workspace began, with their items, and that
    /// user's items in the others. Returns how many conversations and items.
    pub async fn conversations_erase(&self, workspace_id: &str, whose: Whose<'_>) -> Result<(u64, u64), StoreError> {
        let (column, value) = whose.column();
        let mut tx = self.pool().begin().await?;
        let items = sqlx::query(&format!(
            "DELETE FROM conversation_items WHERE workspace_id = $1 AND ({column} = $2
                 OR conversation_id IN (SELECT id FROM conversations WHERE workspace_id = $1 AND {column} = $2))"
        ))
        .bind(workspace_id)
        .bind(value)
        .execute(&mut *tx)
        .await?
        .rows_affected();
        let conversations =
            sqlx::query(&format!("DELETE FROM conversations WHERE workspace_id = $1 AND {column} = $2"))
                .bind(workspace_id)
                .bind(value)
                .execute(&mut *tx)
                .await?
                .rows_affected();
        tx.commit().await?;
        Ok((conversations, items))
    }

    /// Deletes the conversations past their retention, with their items. Returns how many.
    pub async fn conversations_expire(&self) -> Result<u64, StoreError> {
        let mut total = 0;
        loop {
            let done = sqlx::query(
                "DELETE FROM conversations WHERE id IN
                     (SELECT id FROM conversations WHERE expires_at <= now() LIMIT $1)",
            )
            .bind(EXPIRE_ROWS)
            .execute(self.pool())
            .await?
            .rows_affected();
            total += done;
            if done < EXPIRE_ROWS as u64 {
                return Ok(total);
            }
        }
    }
}
