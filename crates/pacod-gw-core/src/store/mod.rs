//! PostgreSQL access. This module holds all SQL of the core.
//!
//! The data plane never calls into it: requests read the in-memory snapshot. Only the
//! management API, the snapshot reloader and the background flushers use the store.

pub mod jobs;
mod tx;

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use sqlx::Row as _;
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};

use crate::model::{Doc, Obj};

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

    /// Applies the migrations in `migrations/`.
    pub async fn migrate(&self) -> Result<(), sqlx::migrate::MigrateError> {
        sqlx::migrate!("./migrations").run(&self.pool).await
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

    /// Changes with `after < version <= upto`, oldest first.
    pub async fn changes_between(&self, after: i64, upto: i64) -> Result<Vec<Change>, StoreError> {
        let rows = sqlx::query(
            "SELECT version, kind, object_id, op FROM config_changes WHERE version > $1 AND version <= $2 ORDER BY version",
        )
        .bind(after)
        .bind(upto)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Change {
                    version: r.try_get("version")?,
                    kind: r.try_get("kind")?,
                    object_id: r.try_get("object_id")?,
                    op: r.try_get("op")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(Into::into)
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

    // --- instances ------------------------------------------------------------------------

    pub async fn instance_heartbeat(
        &self,
        id: &str,
        role: &str,
        hostname: &str,
        software_version: &str,
        applied_version: i64,
        started_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO instances (id, role, hostname, software_version, applied_version, started_at, heartbeat_at)
             VALUES ($1, $2, $3, $4, $5, $6, now())
             ON CONFLICT (id) DO UPDATE SET applied_version = EXCLUDED.applied_version, heartbeat_at = now()",
        )
        .bind(id)
        .bind(role)
        .bind(hostname)
        .bind(software_version)
        .bind(applied_version)
        .bind(started_at)
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Instances seen within the last `alive_seconds`.
    pub async fn instances(&self, alive_seconds: i64) -> Result<Vec<InstanceRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, role, hostname, software_version, applied_version, started_at, heartbeat_at
             FROM instances WHERE heartbeat_at > now() - make_interval(secs => $1) ORDER BY id",
        )
        .bind(alive_seconds as f64)
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|r| {
                Ok(InstanceRow {
                    id: r.try_get("id")?,
                    role: r.try_get("role")?,
                    hostname: r.try_get("hostname")?,
                    software_version: r.try_get("software_version")?,
                    applied_version: r.try_get("applied_version")?,
                    started_at: r.try_get("started_at")?,
                    heartbeat_at: r.try_get("heartbeat_at")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(Into::into)
    }

    pub async fn instance_remove(&self, id: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM instances WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
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
