//! Responses of OpenAI's Responses API that the gateway keeps for a workspace, so that a later
//! call can continue one (`previous_response_id`) and read it back. The provider keeps nothing:
//! the gateway replays the stored turns.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use sqlx::Row;

use super::{Store, StoreError};

/// One stored turn.
#[derive(Debug, Clone, PartialEq)]
pub struct StoredResponse {
    /// `resp_<the call's request id>`.
    pub id: String,
    pub organisation_id: String,
    pub workspace_id: String,
    pub key_id: String,
    pub end_user: String,
    pub user_id: String,
    /// The turn this one continues.
    pub previous_id: Option<String>,
    /// The conversation the turn belongs to, if any.
    pub conversation_id: Option<String>,
    /// The model as the caller named it.
    pub model: String,
    pub status: String,
    /// This turn's input items, a JSON array.
    pub input: Vec<u8>,
    /// The response object as the caller received it.
    pub response: Vec<u8>,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// The turns a response continues, as a call that names it needs them.
#[derive(Debug, Clone, PartialEq)]
pub enum Chain {
    /// The turns, oldest first, the named one last.
    Found(Vec<StoredResponse>),
    /// A turn is not kept (any more): the one named, or one before it that was deleted, has
    /// expired or belongs to another workspace.
    Missing(String),
    /// The turns hold more than the bytes allowed.
    TooLarge,
}

/// Whose records an erasure is for, as the calls named them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Whose<'a> {
    EndUser(&'a str),
    User(&'a str),
}

impl Whose<'_> {
    fn column(&self) -> (&'static str, &str) {
        match self {
            Whose::EndUser(v) => ("end_user", v),
            Whose::User(v) => ("user_id", v),
        }
    }
}

const COLUMNS: &str = "id, organisation_id, workspace_id, key_id, end_user, user_id, previous_id, conversation_id, \
     model, status, input, response, created_at, expires_at";

/// The most rows one statement of the expiry deletes, so that no statement holds its locks long.
const EXPIRE_ROWS: i64 = 5000;

fn response_of(r: &sqlx::postgres::PgRow) -> Result<StoredResponse, sqlx::Error> {
    Ok(StoredResponse {
        id: r.try_get("id")?,
        organisation_id: r.try_get("organisation_id")?,
        workspace_id: r.try_get("workspace_id")?,
        key_id: r.try_get("key_id")?,
        end_user: r.try_get("end_user")?,
        user_id: r.try_get("user_id")?,
        previous_id: r.try_get("previous_id")?,
        conversation_id: r.try_get("conversation_id")?,
        model: r.try_get("model")?,
        status: r.try_get("status")?,
        input: r.try_get("input")?,
        response: r.try_get("response")?,
        created_at: r.try_get("created_at")?,
        expires_at: r.try_get("expires_at")?,
    })
}

impl Store {
    /// Keeps a response. Its id is minted per call, so a second write of the same id is the
    /// same call's and changes nothing.
    pub async fn response_keep(&self, r: &StoredResponse) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO stored_responses (id, organisation_id, workspace_id, key_id, end_user, user_id, previous_id, \
                 conversation_id, model, status, input, response, created_at, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14) ON CONFLICT (id) DO NOTHING",
        )
        .bind(&r.id)
        .bind(&r.organisation_id)
        .bind(&r.workspace_id)
        .bind(&r.key_id)
        .bind(&r.end_user)
        .bind(&r.user_id)
        .bind(&r.previous_id)
        .bind(&r.conversation_id)
        .bind(&r.model)
        .bind(&r.status)
        .bind(&r.input)
        .bind(&r.response)
        .bind(r.created_at)
        .bind(r.expires_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// A workspace's response, unless it has expired.
    pub async fn response(&self, workspace_id: &str, id: &str) -> Result<Option<StoredResponse>, StoreError> {
        let row = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM stored_responses WHERE workspace_id = $1 AND id = $2 AND expires_at > now()"
        ))
        .bind(workspace_id)
        .bind(id)
        .fetch_optional(self.pool())
        .await?;
        Ok(row.as_ref().map(response_of).transpose()?)
    }

    /// The turns that end with the response `id` of a workspace, read only when every one of
    /// them is still kept and together they hold at most `max_bytes`.
    pub async fn response_chain(&self, workspace_id: &str, id: &str, max_bytes: i64) -> Result<Chain, StoreError> {
        // The links first, without the bodies: where the chain breaks, or grows too large, is
        // known before anything large is read. The walk stops once past the bound.
        let links = sqlx::query(
            "WITH RECURSIVE chain AS (
                 SELECT id, previous_id, 1 AS depth, octet_length(input)::int8 + octet_length(response) AS bytes
                 FROM stored_responses WHERE workspace_id = $1 AND id = $2 AND expires_at > now()
                 UNION ALL
                 SELECT r.id, r.previous_id, c.depth + 1, c.bytes + octet_length(r.input) + octet_length(r.response)
                 FROM chain c JOIN stored_responses r ON r.id = c.previous_id
                 WHERE r.workspace_id = $1 AND r.expires_at > now() AND c.bytes <= $3
             )
             SELECT id, previous_id, bytes FROM chain ORDER BY depth",
        )
        .bind(workspace_id)
        .bind(id)
        .bind(max_bytes)
        .fetch_all(self.pool())
        .await?;
        let Some(last) = links.last() else { return Ok(Chain::Missing(id.to_owned())) };
        if last.try_get::<i64, _>("bytes")? > max_bytes {
            return Ok(Chain::TooLarge);
        }
        if let Some(missing) = last.try_get::<Option<String>, _>("previous_id")? {
            return Ok(Chain::Missing(missing));
        }
        let ids: Vec<String> = links.iter().map(|r| r.try_get("id")).collect::<Result<_, _>>()?;
        let rows = sqlx::query(&format!("SELECT {COLUMNS} FROM stored_responses WHERE id = ANY($1)"))
            .bind(&ids)
            .fetch_all(self.pool())
            .await?;
        let mut by_id: HashMap<String, StoredResponse> =
            rows.iter().map(|r| response_of(r).map(|t| (t.id.clone(), t))).collect::<Result<_, _>>()?;
        // Oldest first: the reverse of the walk. A turn deleted between the two reads breaks
        // the chain as if it had been gone before.
        let mut turns = Vec::with_capacity(ids.len());
        for id in ids.iter().rev() {
            match by_id.remove(id) {
                Some(t) => turns.push(t),
                None => return Ok(Chain::Missing(id.clone())),
            }
        }
        Ok(Chain::Found(turns))
    }

    /// Deletes a workspace's response. Returns whether there was one.
    pub async fn response_delete(&self, workspace_id: &str, id: &str) -> Result<bool, StoreError> {
        let done = sqlx::query("DELETE FROM stored_responses WHERE workspace_id = $1 AND id = $2")
            .bind(workspace_id)
            .bind(id)
            .execute(self.pool())
            .await?
            .rows_affected();
        Ok(done == 1)
    }

    /// Deletes the responses one user of a workspace was given. Returns how many.
    pub async fn responses_erase(&self, workspace_id: &str, whose: Whose<'_>) -> Result<u64, StoreError> {
        let (column, value) = whose.column();
        let done = sqlx::query(&format!("DELETE FROM stored_responses WHERE workspace_id = $1 AND {column} = $2"))
            .bind(workspace_id)
            .bind(value)
            .execute(self.pool())
            .await?
            .rows_affected();
        Ok(done)
    }

    /// Deletes the responses past their retention, a bounded number of rows per statement.
    /// Returns how many.
    pub async fn responses_expire(&self) -> Result<u64, StoreError> {
        let mut total = 0;
        loop {
            let done = sqlx::query(
                "DELETE FROM stored_responses WHERE id IN
                     (SELECT id FROM stored_responses WHERE expires_at <= now() LIMIT $1)",
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
