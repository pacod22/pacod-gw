//! The identity audit log.

use super::{Store, StoreError, Tx};

/// One audit row. Never a password or a token in `detail`.
#[derive(Debug, Clone, Default)]
pub struct AuditEntry {
    pub actor: String,
    pub actor_kind: String,
    pub action: String,
    pub subject: String,
    pub organisation_id: Option<String>,
    pub scope_type: Option<String>,
    pub scope_id: Option<String>,
    pub client_id: Option<String>,
    pub realm: Option<String>,
    pub ip: String,
    pub user_agent: String,
    pub detail: serde_json::Value,
}

impl AuditEntry {
    pub fn new(action: &str) -> Self {
        AuditEntry { action: action.to_owned(), detail: serde_json::json!({}), ..Default::default() }
    }

    pub fn by(mut self, actor: &str, kind: &str) -> Self {
        self.actor = actor.to_owned();
        self.actor_kind = kind.to_owned();
        self
    }

    pub fn subject(mut self, subject: &str) -> Self {
        self.subject = subject.to_owned();
        self
    }

    pub fn detail(mut self, detail: serde_json::Value) -> Self {
        self.detail = detail;
        self
    }
}

const INSERT: &str = "INSERT INTO audit_log (actor, actor_kind, action, subject, organisation_id, scope_type, scope_id,
     client_id, realm, ip, user_agent, detail) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)";

macro_rules! bind_entry {
    ($q:expr, $e:expr) => {
        $q.bind(&$e.actor)
            .bind(&$e.actor_kind)
            .bind(&$e.action)
            .bind(&$e.subject)
            .bind(&$e.organisation_id)
            .bind(&$e.scope_type)
            .bind(&$e.scope_id)
            .bind(&$e.client_id)
            .bind(&$e.realm)
            .bind(&$e.ip)
            .bind(&$e.user_agent)
            .bind(&$e.detail)
    };
}

impl Store {
    pub async fn audit(&self, e: &AuditEntry) -> Result<(), StoreError> {
        bind_entry!(sqlx::query(INSERT), e).execute(self.pool()).await?;
        Ok(())
    }

    pub async fn audit_in(tx: &mut Tx, e: &AuditEntry) -> Result<(), StoreError> {
        bind_entry!(sqlx::query(INSERT), e).execute(&mut **tx).await?;
        Ok(())
    }

    /// Deletes audit rows older than `days` (400 days).
    pub async fn audit_prune(&self, days: u32) -> Result<u64, StoreError> {
        Ok(sqlx::query("DELETE FROM audit_log WHERE at < now() - make_interval(days => $1)")
            .bind(i32::try_from(days).unwrap_or(i32::MAX))
            .execute(self.pool())
            .await?
            .rows_affected())
    }
}
