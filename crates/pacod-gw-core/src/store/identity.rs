//! What moving the gateway's people to pacod-id reads and writes here
//! (`crate::identity_migration`).

use chrono::{DateTime, Utc};
use sqlx::Row as _;

use super::{Row, Store, StoreError};

/// What the identity export reads, in one consistent view.
pub struct ExportView {
    pub config_version: i64,
    /// Organisations, workspaces, users, admin tokens, role bindings and the settings.
    pub objects: Vec<Row>,
    pub invitations: Vec<InvitationRow>,
}

/// An invitation as stored, with its link's hash.
pub struct InvitationRow {
    pub id: String,
    pub token_hash: Vec<u8>,
    pub email: String,
    pub role: String,
    pub organisation_id: String,
    pub workspace_id: Option<String>,
    pub invited_by: String,
    pub emailed: bool,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// The kinds of configuration documents that are people, tokens and roles.
pub const IDENTITY_KINDS: [&str; 3] = ["user", "admin_token", "role_binding"];

impl Store {
    pub async fn identity_export_view(&self, kinds: &[&str]) -> Result<ExportView, StoreError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY").execute(&mut *tx).await?;
        let config_version: i64 = sqlx::query_scalar("SELECT version FROM config_head").fetch_one(&mut *tx).await?;
        let kinds: Vec<String> = kinds.iter().map(|k| (*k).to_owned()).collect();
        let objects =
            sqlx::query(&format!("SELECT {} FROM config_objects WHERE kind = ANY($1) ORDER BY id", super::COLUMNS))
                .bind(&kinds)
                .fetch_all(&mut *tx)
                .await?
                .iter()
                .map(Row::from_pg)
                .collect::<Result<Vec<_>, _>>()?;
        let invitations = sqlx::query("SELECT * FROM invitations ORDER BY created_at, id")
            .fetch_all(&mut *tx)
            .await?
            .iter()
            .map(|r| {
                Ok(InvitationRow {
                    id: r.try_get("id")?,
                    token_hash: r.try_get("token_hash")?,
                    email: r.try_get("email")?,
                    role: r.try_get("role")?,
                    organisation_id: r.try_get("organisation_id")?,
                    workspace_id: r.try_get("workspace_id")?,
                    invited_by: r.try_get("invited_by")?,
                    emailed: r.try_get("emailed")?,
                    created_at: r.try_get("created_at")?,
                    expires_at: r.try_get("expires_at")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()?;
        tx.commit().await?;
        Ok(ExportView { config_version, objects, invitations })
    }

    /// When `identity finalize` ran, if it did.
    pub async fn identity_finalized(&self) -> Result<Option<DateTime<Utc>>, StoreError> {
        Ok(sqlx::query_scalar("SELECT finalized_at FROM identity_state").fetch_optional(&self.pool).await?)
    }

    /// People, tokens and roles created, changed or deleted here by someone after configuration
    /// version `after`, and invitations sent after `since`: what an export read at that point
    /// does not know. A deletion matters as much as a creation: pacod-id would keep what the
    /// gateway took away (a leaked token, a former admin's role). Deletions the system made
    /// itself do not count: an organisation's roles going with it (pacod-id removes them too),
    /// an account pacod-id deleted.
    pub async fn identity_written_after(&self, after: i64, since: DateTime<Utc>) -> Result<Vec<String>, StoreError> {
        let kinds: Vec<String> = IDENTITY_KINDS.iter().map(|k| (*k).to_owned()).collect();
        let mut out: Vec<String> = sqlx::query(
            "SELECT kind, object_id, op FROM config_changes WHERE version > $1 AND kind = ANY($2) AND op <> 'delete'
             ORDER BY version",
        )
        .bind(after)
        .bind(&kinds)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| {
            Ok(format!(
                "{} {} {}",
                r.try_get::<String, _>("op")?,
                r.try_get::<String, _>("kind")?,
                r.try_get::<String, _>("object_id")?
            ))
        })
        .collect::<Result<_, sqlx::Error>>()?;
        let deleted: Vec<String> = sqlx::query(
            "SELECT kind, object_id FROM audit_log
             WHERE config_version > $1 AND kind = ANY($2) AND action = 'delete'
               AND actor NOT IN ('system', 'identity-deletion', 'identity-finalize')
             ORDER BY id",
        )
        .bind(after)
        .bind(&kinds)
        .fetch_all(&self.pool)
        .await?
        .iter()
        .map(|r| Ok(format!("delete {} {}", r.try_get::<String, _>("kind")?, r.try_get::<String, _>("object_id")?)))
        .collect::<Result<_, sqlx::Error>>()?;
        out.extend(deleted);
        let sent: Vec<String> =
            sqlx::query_scalar("SELECT id FROM invitations WHERE created_at > $1 ORDER BY created_at")
                .bind(since)
                .fetch_all(&self.pool)
                .await?;
        out.extend(sent.into_iter().map(|id| format!("invitation {id}")));
        Ok(out)
    }

    /// Running instances that serve the management API with the core's own sign-in: they have
    /// heart-beaten within `within_secs` and follow no pacod-id feed.
    pub async fn local_auth_instances(&self, within_secs: f64) -> Result<Vec<String>, StoreError> {
        Ok(sqlx::query_scalar(
            "SELECT id FROM instances WHERE heartbeat_at > now() - make_interval(secs => $1)
               AND role IN ('all', 'admin') AND authz_seq IS NULL ORDER BY id",
        )
        .bind(within_secs)
        .fetch_all(&self.pool)
        .await?)
    }

    /// Whether the gateway still holds its own copy of a principal (until `identity finalize`):
    /// its user or admin token, or a role binding of it.
    pub async fn identity_copy_exists(&self, sub: &str) -> Result<bool, StoreError> {
        Ok(sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM config_objects
                            WHERE (id = $1 AND kind IN ('user', 'admin_token'))
                               OR (kind = 'role_binding' AND doc->>'principal' = $1))",
        )
        .bind(sub)
        .fetch_one(&self.pool)
        .await?)
    }

    /// What stays of an account pacod-id deleted: the records of what it
    /// did, under `deleted account`; records about the account itself keep only that something
    /// happened; its console sessions and one-time links go. One transaction of its own, outside
    /// the configuration's lock.
    pub async fn forget_principal(&self, sub: &str) -> Result<(), StoreError> {
        const GONE: &str = "deleted account";
        let mut tx = self.pool.begin().await?;
        for sql in [
            "UPDATE audit_log SET actor = $2 WHERE actor = $1",
            "UPDATE audit_log SET object_id = $2, detail = '{}'::jsonb WHERE object_id = $1",
            "UPDATE template_versions SET created_by = $2 WHERE created_by = $1",
            "UPDATE invitations SET invited_by = $2 WHERE invited_by = $1",
        ] {
            sqlx::query(sql).bind(sub).bind(GONE).execute(&mut *tx).await?;
        }
        for sql in ["DELETE FROM sessions WHERE user_id = $1", "DELETE FROM account_tokens WHERE user_id = $1"] {
            sqlx::query(sql).bind(sub).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
