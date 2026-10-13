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

/// An organisation id and a workspace id, either of them absent.
type Scope = (Option<String>, Option<String>);

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

    async fn bump(&mut self, kind: &str, id: &str, op: &str, scope: Scope) -> Result<i64, StoreError> {
        let v: i64 = sqlx::query("UPDATE config_head SET version = version + 1 RETURNING version")
            .fetch_one(&mut *self.tx)
            .await?
            .try_get(0)?;
        sqlx::query(
            "INSERT INTO config_changes (version, kind, object_id, op, organisation_id, workspace_id)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(v)
        .bind(kind)
        .bind(id)
        .bind(op)
        .bind(scope.0)
        .bind(scope.1)
        .execute(&mut *self.tx)
        .await?;
        self.last_version = Some(v);
        Ok(v)
    }

    /// The organisation and workspace an object belongs to, for the change log: its own id for
    /// an organisation or a workspace, the scope a credential or role binding names, otherwise
    /// the organisation or workspace it hangs under. Falls back to the audit entry's scope.
    async fn scope_of(
        &mut self,
        kind: &str,
        id: &str,
        parent: Option<&str>,
        doc: &serde_json::Value,
        audit: &AuditEntry,
    ) -> Result<Scope, StoreError> {
        let found = match kind {
            "organisation" => (Some(id.to_owned()), None),
            "workspace" => (parent.map(str::to_owned), Some(id.to_owned())),
            _ => match doc.get("scope").filter(|s| s.is_object()) {
                Some(scope) => {
                    let sid = scope.get("id").and_then(serde_json::Value::as_str);
                    match (scope.get("type").and_then(serde_json::Value::as_str), sid) {
                        (Some("organisation"), Some(o)) => (Some(o.to_owned()), None),
                        (Some("workspace"), Some(w)) => self.scope_under(w).await?,
                        _ => (None, None),
                    }
                }
                None => match parent {
                    Some(p) => self.scope_under(p).await?,
                    None => (None, None),
                },
            },
        };
        Ok(if found.0.is_some() { found } else { (audit.organisation_id.clone(), audit.workspace_id.clone()) })
    }

    /// Keeps every version of a template in `template_versions`: the document holds only the
    /// recent ones. A version already there is left as it is.
    async fn archive_versions(&mut self, kind: &str, id: &str, doc: &serde_json::Value) -> Result<(), StoreError> {
        if kind != "template" {
            return Ok(());
        }
        for v in doc.get("versions").and_then(|v| v.as_array()).into_iter().flatten() {
            let Some(version) = v.get("version").and_then(serde_json::Value::as_i64) else { continue };
            sqlx::query(
                "INSERT INTO template_versions (template_id, version, spec, comment, created_by, created_at)
                 VALUES ($1, $2, $3, $4, $5, coalesce($6::timestamptz, now())) ON CONFLICT DO NOTHING",
            )
            .bind(id)
            .bind(version as i32)
            .bind(v.get("spec").cloned().unwrap_or_default())
            .bind(v.get("comment").and_then(|c| c.as_str()).unwrap_or_default())
            .bind(v.get("created_by").and_then(|c| c.as_str()).unwrap_or_default())
            .bind(v.get("created_at").and_then(|c| c.as_str()))
            .execute(&mut *self.tx)
            .await?;
        }
        Ok(())
    }

    /// Every archived version of a template, newest first, without their specs.
    pub async fn template_versions(&mut self, id: &str) -> Result<Vec<serde_json::Value>, StoreError> {
        let rows = sqlx::query(
            "SELECT json_build_object('version', version, 'comment', comment, 'created_by', created_by,
                    'created_at', created_at) AS v
             FROM template_versions WHERE template_id = $1 ORDER BY version DESC",
        )
        .bind(id)
        .fetch_all(&mut *self.tx)
        .await?;
        Ok(rows.iter().map(|r| r.try_get::<serde_json::Value, _>("v")).collect::<Result<Vec<_>, _>>()?)
    }

    /// One archived version of a template.
    pub async fn template_version(&mut self, id: &str, version: u32) -> Result<Option<serde_json::Value>, StoreError> {
        let row = sqlx::query(
            "SELECT json_build_object('version', version, 'spec', spec, 'comment', comment, 'created_by', created_by,
                    'created_at', created_at) AS v
             FROM template_versions WHERE template_id = $1 AND version = $2",
        )
        .bind(id)
        .bind(version as i32)
        .fetch_optional(&mut *self.tx)
        .await?;
        Ok(row.map(|r| r.try_get::<serde_json::Value, _>("v")).transpose()?)
    }

    /// The scope of something whose parent is `parent`: an organisation, a workspace, or
    /// a global object such as a model.
    async fn scope_under(&mut self, parent: &str) -> Result<Scope, StoreError> {
        Ok(match self.get(parent).await? {
            Some(r) if r.kind == "organisation" => (Some(r.id), None),
            Some(r) if r.kind == "workspace" => (r.parent, Some(r.id)),
            _ => (None, None),
        })
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

    /// Queues a change for pacod-id (the identity outbox, migration 0014), committed with the
    /// change it reflects.
    pub async fn idp_enqueue(&mut self, op: &str, target: &str, payload: &serde_json::Value) -> Result<(), StoreError> {
        super::idp::enqueue(&mut self.tx, op, target, payload).await
    }

    /// What stays of an account pacod-id deleted (identity concept 6.9): the records of what it
    /// did, under `deleted account`. Records about the account itself keep only that something
    /// happened; its console sessions and one-time links go.
    pub async fn forget_principal(&mut self, sub: &str) -> Result<(), StoreError> {
        const GONE: &str = "deleted account";
        for sql in [
            "UPDATE audit_log SET actor = $2 WHERE actor = $1",
            "UPDATE audit_log SET object_id = $2, detail = '{}'::jsonb WHERE object_id = $1",
            "UPDATE template_versions SET created_by = $2 WHERE created_by = $1",
            "UPDATE invitations SET invited_by = $2 WHERE invited_by = $1",
        ] {
            sqlx::query(sql).bind(sub).bind(GONE).execute(&mut *self.tx).await?;
        }
        for sql in ["DELETE FROM sessions WHERE user_id = $1", "DELETE FROM account_tokens WHERE user_id = $1"] {
            sqlx::query(sql).bind(sub).execute(&mut *self.tx).await?;
        }
        Ok(())
    }

    /// The last step of `identity finalize`, after the documents are deleted: the invitations,
    /// console sessions and one-time links go, and the core no longer starts in local mode.
    pub async fn identity_finalize(&mut self, import: &serde_json::Value) -> Result<(), StoreError> {
        for table in ["invitations", "sessions", "account_tokens"] {
            sqlx::query(&format!("DELETE FROM {table}")).execute(&mut *self.tx).await?;
        }
        sqlx::query("INSERT INTO identity_state (import) VALUES ($1)").bind(import).execute(&mut *self.tx).await?;
        Ok(())
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
        self.archive_versions(kind, id, doc).await?;
        let scope = self.scope_of(kind, id, parent, doc, &audit).await?;
        let v = self.bump(kind, id, "create", scope).await?;
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
        self.archive_versions(&before.kind, id, doc).await?;
        let scope = self.scope_of(&before.kind, id, before.parent.as_deref(), doc, &audit).await?;
        let v = self.bump(&before.kind, id, "update", scope).await?;
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
        // Before the row goes: a workspace being deleted is still there to be looked up.
        let scope = self.scope_of(&before.kind, id, before.parent.as_deref(), &before.doc, &audit).await?;
        sqlx::query("DELETE FROM config_objects WHERE id = $1").bind(id).execute(&mut *self.tx).await?;
        let v = self.bump(&before.kind, id, "delete", scope).await?;
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
