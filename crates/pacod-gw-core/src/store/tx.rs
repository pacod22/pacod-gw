//! A configuration change: object writes, their audit entries and the version increments,
//! all in one transaction (concept section 11.2).

use sqlx::postgres::PgPool;
use sqlx::{Postgres, Row as _, Transaction};

use super::{COLUMNS, Row, StoreError};

/// An entry for the audit log.
#[derive(Debug, Clone, Default)]
pub struct AuditEntry {
    pub actor: String,
    /// `user`, `token`, `internal` or `system`.
    pub actor_kind: String,
    pub action: String,
    pub kind: String,
    pub object_id: String,
    pub organisation_id: Option<String>,
    pub workspace_id: Option<String>,
    pub before_hash: Option<String>,
    pub after_hash: Option<String>,
    pub detail: serde_json::Value,
}

impl AuditEntry {
    /// An entry for something the gateway did itself, not a person.
    pub fn system(actor: &str) -> Self {
        Self { actor: actor.to_owned(), actor_kind: "system".into(), ..Default::default() }
    }
}

pub(super) async fn insert_audit<'e, E>(exec: E, e: &AuditEntry, config_version: Option<i64>) -> Result<(), StoreError>
where
    E: sqlx::Executor<'e, Database = Postgres>,
{
    let detail = if e.detail.is_null() { serde_json::json!({}) } else { e.detail.clone() };
    sqlx::query(
        "INSERT INTO audit_log (actor, actor_kind, action, kind, object_id, organisation_id, workspace_id,
                                before_hash, after_hash, config_version, detail)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)",
    )
    .bind(&e.actor)
    .bind(&e.actor_kind)
    .bind(&e.action)
    .bind(&e.kind)
    .bind(&e.object_id)
    .bind(&e.organisation_id)
    .bind(&e.workspace_id)
    .bind(&e.before_hash)
    .bind(&e.after_hash)
    .bind(config_version)
    .bind(detail)
    .execute(exec)
    .await?;
    Ok(())
}

/// SHA-256 of a document, hex. Recorded in the audit log before and after a change.
pub fn doc_hash(doc: &serde_json::Value) -> String {
    hex::encode(crate::crypto::sha256(doc.to_string().as_bytes()))
}

pub struct Tx {
    tx: Transaction<'static, Postgres>,
    /// The last version this transaction produced, if it changed anything.
    last_version: Option<i64>,
}

impl Tx {
    pub(super) async fn begin(pool: &PgPool) -> Result<Self, StoreError> {
        let mut tx = pool.begin().await?;
        // Every configuration change holds this row lock until it commits. Versions therefore
        // become visible in order, and a poller that has seen version n has seen every change
        // up to n.
        sqlx::query("SELECT version FROM config_head FOR UPDATE").execute(&mut *tx).await?;
        Ok(Self { tx, last_version: None })
    }

    async fn bump(&mut self, kind: &str, id: &str, op: &str) -> Result<i64, StoreError> {
        let v: i64 = sqlx::query("UPDATE config_head SET version = version + 1 RETURNING version")
            .fetch_one(&mut *self.tx)
            .await?
            .try_get(0)?;
        sqlx::query("INSERT INTO config_changes (version, kind, object_id, op) VALUES ($1, $2, $3, $4)")
            .bind(v)
            .bind(kind)
            .bind(id)
            .bind(op)
            .execute(&mut *self.tx)
            .await?;
        self.last_version = Some(v);
        Ok(v)
    }

    pub async fn get(&mut self, id: &str) -> Result<Option<Row>, StoreError> {
        let r = sqlx::query(&format!("SELECT {COLUMNS} FROM config_objects WHERE id = $1"))
            .bind(id)
            .fetch_optional(&mut *self.tx)
            .await?;
        Ok(r.as_ref().map(Row::from_pg).transpose()?)
    }

    /// The object, when it exists and has the given kind.
    pub async fn get_kind(&mut self, kind: &str, id: &str) -> Result<Option<Row>, StoreError> {
        Ok(self.get(id).await?.filter(|r| r.kind == kind))
    }

    /// All objects of one kind, optionally of one parent.
    pub async fn list(&mut self, kind: &str, parent: Option<&str>) -> Result<Vec<Row>, StoreError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM config_objects WHERE kind = $1 AND ($2::text IS NULL OR parent = $2) ORDER BY id"
        ))
        .bind(kind)
        .bind(parent)
        .fetch_all(&mut *self.tx)
        .await?;
        Ok(rows.iter().map(Row::from_pg).collect::<Result<Vec<_>, _>>()?)
    }

    /// How many objects have `parent` as their parent, across kinds.
    pub async fn count_children(&mut self, parent: &str) -> Result<i64, StoreError> {
        Ok(sqlx::query("SELECT count(*) FROM config_objects WHERE parent = $1")
            .bind(parent)
            .fetch_one(&mut *self.tx)
            .await?
            .try_get(0)?)
    }

    /// Withdraws the invitations to an organisation, or to one workspace, being deleted.
    pub async fn invitations_delete_for_scope(&mut self, org: &str, ws: Option<&str>) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM invitations WHERE organisation_id = $1 AND ($2::text IS NULL OR workspace_id = $2)")
            .bind(org)
            .bind(ws)
            .execute(&mut *self.tx)
            .await?;
        Ok(())
    }

    pub async fn count_kind(&mut self, kind: &str) -> Result<i64, StoreError> {
        Ok(sqlx::query("SELECT count(*) FROM config_objects WHERE kind = $1")
            .bind(kind)
            .fetch_one(&mut *self.tx)
            .await?
            .try_get(0)?)
    }

    /// Inserts an object and records the change and its audit entry.
    pub async fn create(
        &mut self,
        kind: &str,
        id: &str,
        parent: Option<&str>,
        name: Option<&str>,
        doc: &serde_json::Value,
        mut audit: AuditEntry,
    ) -> Result<Row, StoreError> {
        let inserted = sqlx::query(&format!(
            "INSERT INTO config_objects (id, kind, parent, name, doc) VALUES ($1, $2, $3, $4, $5) RETURNING {COLUMNS}"
        ))
        .bind(id)
        .bind(kind)
        .bind(parent)
        .bind(name)
        .bind(doc)
        .fetch_one(&mut *self.tx)
        .await
        .map_err(|e| unique_to_conflict(e, kind, name))?;
        let row = Row::from_pg(&inserted)?;
        let v = self.bump(kind, id, "create").await?;
        audit.action = if audit.action.is_empty() { "create".into() } else { audit.action };
        audit.kind = kind.to_owned();
        audit.object_id = id.to_owned();
        audit.after_hash = Some(doc_hash(doc));
        insert_audit(&mut *self.tx, &audit, Some(v)).await?;
        Ok(row)
    }

    /// Replaces an object's document. `expected_version`, when given, must match the stored
    /// version.
    pub async fn update(
        &mut self,
        id: &str,
        expected_version: Option<i32>,
        name: Option<&str>,
        doc: &serde_json::Value,
        mut audit: AuditEntry,
    ) -> Result<Row, StoreError> {
        let before = self.get(id).await?.ok_or(StoreError::NotFound)?;
        if expected_version.is_some_and(|v| v != before.version) {
            return Err(StoreError::VersionMismatch);
        }
        let updated = sqlx::query(&format!(
            "UPDATE config_objects SET name = $2, doc = $3, version = version + 1, updated_at = now()
             WHERE id = $1 RETURNING {COLUMNS}"
        ))
        .bind(id)
        .bind(name)
        .bind(doc)
        .fetch_one(&mut *self.tx)
        .await
        .map_err(|e| unique_to_conflict(e, &before.kind, name))?;
        let row = Row::from_pg(&updated)?;
        let v = self.bump(&before.kind, id, "update").await?;
        audit.action = if audit.action.is_empty() { "update".into() } else { audit.action };
        audit.kind = before.kind.clone();
        audit.object_id = id.to_owned();
        audit.before_hash = Some(doc_hash(&before.doc));
        audit.after_hash = Some(doc_hash(doc));
        insert_audit(&mut *self.tx, &audit, Some(v)).await?;
        Ok(row)
    }

    pub async fn delete(&mut self, id: &str, mut audit: AuditEntry) -> Result<Row, StoreError> {
        let before = self.get(id).await?.ok_or(StoreError::NotFound)?;
        sqlx::query("DELETE FROM config_objects WHERE id = $1").bind(id).execute(&mut *self.tx).await?;
        let v = self.bump(&before.kind, id, "delete").await?;
        audit.action = if audit.action.is_empty() { "delete".into() } else { audit.action };
        audit.kind = before.kind.clone();
        audit.object_id = id.to_owned();
        audit.before_hash = Some(doc_hash(&before.doc));
        insert_audit(&mut *self.tx, &audit, Some(v)).await?;
        Ok(before)
    }

    /// Commits. Returns the configuration version the change produced, or `None` when the
    /// transaction changed nothing.
    pub async fn commit(self) -> Result<Option<i64>, StoreError> {
        self.tx.commit().await?;
        Ok(self.last_version)
    }
}

fn unique_to_conflict(e: sqlx::Error, kind: &str, name: Option<&str>) -> StoreError {
    match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => match name {
            Some(n) => StoreError::Conflict(format!("a {kind} named \"{n}\" already exists here")),
            None => StoreError::Conflict(format!("this {kind} already exists")),
        },
        _ => StoreError::Db(e),
    }
}
