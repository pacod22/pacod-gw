//! PostgreSQL access. This module holds all SQL of the core.
//!
//! The data plane never calls into it: requests read the in-memory snapshot. Only the
//! management API, the snapshot reloader and the background flushers use the store.

pub mod conversations;
pub mod discovery;
pub mod identity;
pub mod idp;
mod instances;
mod invitations;
pub mod jobs;
pub mod objects;
pub mod responses;
mod tx;

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use sqlx::Row as _;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};

use crate::model::{Doc, Obj};

pub use instances::{Heartbeat, InstanceRecord};
pub use invitations::{Invitation, NewInvitation};
pub use tx::{AuditEntry, Tx, doc_hash};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found")]
    NotFound,
    /// A unique name is already taken.
    #[error("{0}")]
    Conflict(String),
    /// The object changed since the caller read it.
    #[error("the object was changed by someone else; read it again and retry")]
    VersionMismatch,
    #[error("stored document of kind {kind} with id {id} does not match its type: {source}")]
    Corrupt { kind: String, id: String, source: serde_json::Error },
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    /// A store other than the database (the minimal gateway's file of objects) failed.
    #[error("{0}")]
    Io(String),
}

/// One row of `config_objects`.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub id: String,
    pub kind: String,
    pub parent: Option<String>,
    pub name: Option<String>,
    pub doc: serde_json::Value,
    pub version: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl Row {
    fn from_pg(r: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: r.try_get("id")?,
            kind: r.try_get("kind")?,
            parent: r.try_get("parent")?,
            name: r.try_get("name")?,
            doc: r.try_get("doc")?,
            version: r.try_get("version")?,
            created_at: r.try_get("created_at")?,
            updated_at: r.try_get("updated_at")?,
        })
    }

    /// Decodes the document into its type.
    pub fn into_obj<T: DeserializeOwned>(self) -> Result<Obj<T>, StoreError> {
        let doc = serde_json::from_value(self.doc).map_err(|source| StoreError::Corrupt {
            kind: self.kind.clone(),
            id: self.id.clone(),
            source,
        })?;
        Ok(Obj {
            id: self.id,
            parent: self.parent,
            name: self.name,
            version: self.version,
            created_at: self.created_at,
            updated_at: self.updated_at,
            doc,
        })
    }
}

/// One entry of the change log.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Change {
    pub version: i64,
    pub kind: String,
    pub object_id: String,
    pub op: String,
    /// The organisation the object belongs to; `None` for global objects.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub organisation_id: Option<String>,
    /// The workspace the object belongs to, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

impl Change {
    /// The "refresh everything" notice, for a gap in the change log.
    pub fn reload(version: i64) -> Self {
        Self {
            version,
            kind: "*".into(),
            object_id: "*".into(),
            op: "reload".into(),
            organisation_id: None,
            workspace_id: None,
        }
    }

    fn from_pg(r: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            version: r.try_get("version")?,
            kind: r.try_get("kind")?,
            object_id: r.try_get("object_id")?,
            op: r.try_get("op")?,
            organisation_id: r.try_get("organisation_id")?,
            workspace_id: r.try_get("workspace_id")?,
        })
    }
}

/// What changed in the configuration since a version; see [`Store::load_since`].
#[derive(Debug)]
pub struct Delta {
    pub version: i64,
    pub changes: Vec<Change>,
    /// The current rows of the changed objects that still exist.
    pub rows: Vec<Row>,
}

/// What a listing sorts by.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortKey {
    #[default]
    Id,
    Name,
    Created,
    Updated,
}

/// How a listing is filtered and sorted.
#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub sort: SortKey,
    pub descending: bool,
    /// Objects whose name contains this, in any case.
    pub name_contains: Option<String>,
    /// Objects whose `status` (or, for models, `state`) is this.
    pub status: Option<String>,
}

/// Where a page of a listing starts: after the object with this sort value and id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListCursor {
    pub value: String,
    pub id: String,
}

impl ListCursor {
    /// The cursor after `row` in a listing sorted by `sort`, as the API hands it out.
    pub fn after(row: &Row, sort: SortKey) -> String {
        use base64::Engine;
        let value = match sort {
            SortKey::Id => row.id.clone(),
            SortKey::Name => row.name.clone().unwrap_or_default(),
            SortKey::Created => row.created_at.to_rfc3339(),
            SortKey::Updated => row.updated_at.to_rfc3339(),
        };
        if sort == SortKey::Id {
            return value;
        }
        let text = serde_json::json!([value, row.id]).to_string();
        format!("c1.{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text))
    }

    /// Reads a cursor the API handed out. A plain id is a cursor of a listing by id.
    pub fn parse(text: &str) -> Option<Self> {
        use base64::Engine;
        let Some(encoded) = text.strip_prefix("c1.") else {
            return Some(Self { value: text.to_owned(), id: text.to_owned() });
        };
        let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(encoded).ok()?;
        let [value, id]: [String; 2] = serde_json::from_slice(&bytes).ok()?;
        Some(Self { value, id })
    }
}

/// Which parent a listing is restricted to.
#[derive(Debug, Clone, Copy)]
pub enum ParentFilter<'a> {
    /// Objects of any parent.
    Any,
    /// Objects without a parent.
    None,
    /// Objects of this parent.
    Is(&'a str),
}

/// A running core instance as recorded in the database.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct InstanceRow {
    pub id: String,
    pub role: String,
    pub hostname: String,
    pub software_version: String,
    pub applied_version: i64,
    pub started_at: DateTime<Utc>,
    pub heartbeat_at: DateTime<Utc>,
    /// The last time another running process presented this instance's id.
    pub conflict_at: Option<DateTime<Utc>>,
}

/// One entry of the audit log as returned by the API.
#[derive(Debug, Clone, serde::Serialize, schemars::JsonSchema)]
pub struct AuditRow {
    pub id: i64,
    pub at: DateTime<Utc>,
    pub actor: String,
    pub actor_kind: String,
    pub action: String,
    pub kind: String,
    pub object_id: String,
    pub organisation_id: Option<String>,
    pub workspace_id: Option<String>,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
    pub config_version: Option<i64>,
    pub detail: serde_json::Value,
}

/// What a one-time account token, sent by email, is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TokenPurpose {
    VerifyEmail,
    ResetPassword,
}

impl TokenPurpose {
    pub fn as_str(self) -> &'static str {
        match self {
            TokenPurpose::VerifyEmail => "verify_email",
            TokenPurpose::ResetPassword => "reset_password",
        }
    }
}

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

const COLUMNS: &str = "id, kind, parent, name, doc, version, created_at, updated_at";

impl Store {
    pub async fn connect(url: &str, max_connections: u32) -> Result<Self, sqlx::Error> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect(url)
            .await?;
        Ok(Self { pool })
    }

    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Applies the migrations in `migrations/`. A migration this binary does not know is
    /// accepted: during a rolling upgrade, an instance of the previous version may restart
    /// after a newer one has extended the schema. Migrations are therefore additive.
    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        let mut migrator = sqlx::migrate!("./migrations");
        migrator.set_ignore_missing(true);
        migrator.run(&self.pool).await
    }

    /// True when the database answers.
    pub async fn ping(&self) -> bool {
        sqlx::query("SELECT 1").execute(&self.pool).await.is_ok()
    }

    pub async fn head_version(&self) -> Result<i64, StoreError> {
        Ok(sqlx::query("SELECT version FROM config_head").fetch_one(&self.pool).await?.try_get(0)?)
    }

    /// Reads the whole configuration at one consistent version.
    pub async fn load_all(&self) -> Result<(i64, Vec<Row>), StoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
        let version: i64 = sqlx::query("SELECT version FROM config_head").fetch_one(&mut *tx).await?.try_get(0)?;
        let rows = sqlx::query(&format!("SELECT {COLUMNS} FROM config_objects ORDER BY id"))
            .fetch_all(&mut *tx)
            .await?
            .iter()
            .map(Row::from_pg)
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await?;
        Ok((version, rows))
    }

    /// What changed since `after`, read in one consistent view: the head version, the changes
    /// up to it, and the current rows of the objects they name (a deleted object has none).
    /// `None` when the log cannot say: the head is older than `after` (a restore) or a version
    /// is missing from the log. The caller then loads everything.
    pub async fn load_since(&self, after: i64) -> Result<Option<Delta>, StoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
        let version: i64 = sqlx::query("SELECT version FROM config_head").fetch_one(&mut *tx).await?.try_get(0)?;
        if version < after {
            return Ok(None);
        }
        let changes = sqlx::query(
            "SELECT version, kind, object_id, op, organisation_id, workspace_id FROM config_changes
             WHERE version > $1 AND version <= $2 ORDER BY version",
        )
        .bind(after)
        .bind(version)
        .fetch_all(&mut *tx)
        .await?
        .iter()
        .map(Change::from_pg)
        .collect::<Result<Vec<_>, _>>()?;
        if changes.len() as i64 != version - after {
            return Ok(None);
        }
        let mut ids: Vec<&str> = changes.iter().map(|c| c.object_id.as_str()).collect();
        ids.sort_unstable();
        ids.dedup();
        let rows = sqlx::query(&format!("SELECT {COLUMNS} FROM config_objects WHERE id = ANY($1) ORDER BY id"))
            .bind(&ids)
            .fetch_all(&mut *tx)
            .await?
            .iter()
            .map(Row::from_pg)
            .collect::<Result<Vec<_>, _>>()?;
        tx.commit().await?;
        Ok(Some(Delta { version, changes, rows }))
    }

    /// Changes with `after < version <= upto`, oldest first.
    pub async fn changes_between(&self, after: i64, upto: i64) -> Result<Vec<Change>, StoreError> {
        let rows = sqlx::query(
            "SELECT version, kind, object_id, op, organisation_id, workspace_id FROM config_changes
             WHERE version > $1 AND version <= $2 ORDER BY version",
        )
        .bind(after)
        .bind(upto)
        .fetch_all(&self.pool)
        .await?;
        rows.iter().map(Change::from_pg).collect::<Result<Vec<_>, sqlx::Error>>().map_err(Into::into)
    }

    /// The most recent changes, newest first.
    pub async fn recent_changes(&self, limit: i64) -> Result<Vec<Change>, StoreError> {
        let head = self.head_version().await?;
        let mut v = self.changes_between((head - limit).max(0), head).await?;
        v.reverse();
        Ok(v)
    }

    pub async fn get(&self, id: &str) -> Result<Option<Row>, StoreError> {
        let r = sqlx::query(&format!("SELECT {COLUMNS} FROM config_objects WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.as_ref().map(Row::from_pg).transpose()?)
    }

    /// Reads one object of a known type. `NotFound` when absent or of another kind.
    pub async fn get_obj<T: Doc>(&self, id: &str) -> Result<Obj<T>, StoreError> {
        match self.get(id).await? {
            Some(row) if row.kind == T::KIND => row.into_obj(),
            _ => Err(StoreError::NotFound),
        }
    }

    /// Lists objects of one kind ordered by id, starting after `after_id`.
    pub async fn list(
        &self,
        kind: &str,
        parent: ParentFilter<'_>,
        after_id: Option<&str>,
        limit: i64,
    ) -> Result<Vec<Row>, StoreError> {
        let (parent_sql, parent_val) = match parent {
            ParentFilter::Any => ("TRUE", None),
            ParentFilter::None => ("parent IS NULL", None),
            ParentFilter::Is(p) => ("parent = $4", Some(p)),
        };
        let sql = format!(
            "SELECT {COLUMNS} FROM config_objects WHERE kind = $1 AND ($2::text IS NULL OR id > $2) AND {parent_sql} ORDER BY id LIMIT $3"
        );
        let mut q = sqlx::query(&sql).bind(kind).bind(after_id).bind(limit);
        if let Some(p) = parent_val {
            q = q.bind(p);
        }
        Ok(q.fetch_all(&self.pool).await?.iter().map(Row::from_pg).collect::<Result<Vec<_>, _>>()?)
    }

    /// One page of objects of a kind, filtered and sorted (concept section 10.1). The cursor is
    /// the sort value and the id of the last object of the previous page.
    pub async fn list_page(
        &self,
        kind: &str,
        parent: ParentFilter<'_>,
        filter: &ListFilter,
        cursor: Option<&ListCursor>,
        limit: i64,
    ) -> Result<Vec<Row>, StoreError> {
        let (column, cast) = match filter.sort {
            SortKey::Id => ("id", "text"),
            SortKey::Name => ("coalesce(name, '')", "text"),
            SortKey::Created => ("created_at", "timestamptz"),
            SortKey::Updated => ("updated_at", "timestamptz"),
        };
        let (op, order) = if filter.descending { ("<", "DESC") } else { (">", "ASC") };
        let parent_sql = match parent {
            ParentFilter::Any => "TRUE",
            ParentFilter::None => "parent IS NULL",
            ParentFilter::Is(_) => "parent = $7",
        };
        let sql = format!(
            "SELECT {COLUMNS} FROM config_objects
             WHERE kind = $1 AND {parent_sql}
               AND ($2::text IS NULL OR ({column}, id) {op} ($2::{cast}, $3))
               AND ($4::text IS NULL OR coalesce(name, '') ILIKE '%' || $4 || '%' ESCAPE '\\')
               AND ($5::text IS NULL OR doc->>'status' = $5 OR doc->>'state' = $5)
             ORDER BY {column} {order}, id {order} LIMIT $6"
        );
        let contains =
            filter.name_contains.as_deref().map(|q| q.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        let mut q = sqlx::query(&sql)
            .bind(kind)
            .bind(cursor.map(|c| c.value.clone()))
            .bind(cursor.map(|c| c.id.clone()))
            .bind(contains)
            .bind(filter.status.clone())
            .bind(limit);
        if let ParentFilter::Is(p) = parent {
            q = q.bind(p);
        }
        Ok(q.fetch_all(&self.pool).await?.iter().map(Row::from_pg).collect::<Result<Vec<_>, _>>()?)
    }

    /// Starts a configuration change. Takes the lock that serialises all changes.
    pub async fn begin(&self) -> Result<Tx, StoreError> {
        Tx::begin(&self.pool).await
    }

    // --- sessions -------------------------------------------------------------------------

    pub async fn session_create(
        &self,
        token_hash: &[u8],
        user_id: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO sessions (token_hash, user_id, expires_at) VALUES ($1, $2, $3)")
            .bind(token_hash)
            .bind(user_id)
            .bind(expires_at)
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// The user of a session that has not expired.
    pub async fn session_user(&self, token_hash: &[u8]) -> Result<Option<String>, StoreError> {
        let r = sqlx::query("SELECT user_id FROM sessions WHERE token_hash = $1 AND expires_at > now()")
            .bind(token_hash)
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.map(|r| r.try_get(0)).transpose()?)
    }

    pub async fn session_delete(&self, token_hash: &[u8]) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM sessions WHERE token_hash = $1").bind(token_hash).execute(&self.pool).await?;
        Ok(())
    }

    /// Ends every session of a user, for example after a password change.
    pub async fn sessions_delete_for_user(&self, user_id: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM sessions WHERE user_id = $1").bind(user_id).execute(&self.pool).await?;
        Ok(())
    }

    pub async fn sessions_prune(&self) -> Result<u64, StoreError> {
        Ok(sqlx::query("DELETE FROM sessions WHERE expires_at <= now()").execute(&self.pool).await?.rows_affected())
    }

    // --- account tokens -------------------------------------------------------------------

    /// Stores a one-time token for a user and purpose, replacing the user's previous one.
    pub async fn account_token_create(
        &self,
        token_hash: &[u8],
        purpose: TokenPurpose,
        user_id: &str,
        email: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("DELETE FROM account_tokens WHERE user_id = $1 AND purpose = $2")
            .bind(user_id)
            .bind(purpose.as_str())
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO account_tokens (token_hash, purpose, user_id, email, expires_at) VALUES ($1, $2, $3, $4, $5)",
        )
        .bind(token_hash)
        .bind(purpose.as_str())
        .bind(user_id)
        .bind(email)
        .bind(expires_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Uses up a token: returns the user id and email it was issued for, once, while it is
    /// valid. A second use, an expired token and a token of another purpose all give `None`.
    pub async fn account_token_consume(
        &self,
        token_hash: &[u8],
        purpose: TokenPurpose,
    ) -> Result<Option<(String, String)>, StoreError> {
        let r = sqlx::query(
            "DELETE FROM account_tokens WHERE token_hash = $1 AND purpose = $2 AND expires_at > now()
             RETURNING user_id, email",
        )
        .bind(token_hash)
        .bind(purpose.as_str())
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.map(|r| Ok::<_, sqlx::Error>((r.try_get(0)?, r.try_get(1)?))).transpose()?)
    }

    /// Withdraws a user's outstanding tokens of one purpose.
    pub async fn account_tokens_delete_for_user(&self, user_id: &str, purpose: TokenPurpose) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM account_tokens WHERE user_id = $1 AND purpose = $2")
            .bind(user_id)
            .bind(purpose.as_str())
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    pub async fn account_tokens_prune(&self) -> Result<u64, StoreError> {
        Ok(sqlx::query("DELETE FROM account_tokens WHERE expires_at <= now()")
            .execute(&self.pool)
            .await?
            .rows_affected())
    }

    // --- audit log ------------------------------------------------------------------------

    /// Writes an audit entry outside a configuration change, for example a login.
    pub async fn audit(&self, e: &AuditEntry) -> Result<(), StoreError> {
        tx::insert_audit(&self.pool, e, None).await
    }

    /// Audit entries newest first, starting before `before_id`. Scope filters restrict the
    /// result to one organisation or workspace.
    pub async fn audit_list(
        &self,
        before_id: Option<i64>,
        limit: i64,
        organisation_id: Option<&str>,
        workspace_id: Option<&str>,
    ) -> Result<Vec<AuditRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, at, actor, actor_kind, action, kind, object_id, organisation_id, workspace_id,
                    before_hash, after_hash, config_version, detail
             FROM audit_log
             WHERE ($1::bigint IS NULL OR id < $1)
               AND ($3::text IS NULL OR organisation_id = $3)
               AND ($4::text IS NULL OR workspace_id = $4)
             ORDER BY id DESC LIMIT $2",
        )
        .bind(before_id)
        .bind(limit)
        .bind(organisation_id)
        .bind(workspace_id)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(AuditRow {
                    id: r.try_get("id")?,
                    at: r.try_get("at")?,
                    actor: r.try_get("actor")?,
                    actor_kind: r.try_get("actor_kind")?,
                    action: r.try_get("action")?,
                    kind: r.try_get("kind")?,
                    object_id: r.try_get("object_id")?,
                    organisation_id: r.try_get("organisation_id")?,
                    workspace_id: r.try_get("workspace_id")?,
                    before_hash: r.try_get("before_hash")?,
                    after_hash: r.try_get("after_hash")?,
                    config_version: r.try_get("config_version")?,
                    detail: r.try_get("detail")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(Into::into)
    }
}
