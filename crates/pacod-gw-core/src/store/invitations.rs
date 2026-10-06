//! Pending invitations to an organisation or a workspace (`invitations`).

use chrono::{DateTime, Utc};
use sqlx::Row as _;
use sqlx::postgres::PgRow;

use super::{Store, StoreError};

/// How long an expired invitation is still listed, so that the inviter sees it expired.
const KEEP_EXPIRED_DAYS: i64 = 30;

const COLUMNS: &str = "id, email, role, organisation_id, workspace_id, invited_by, emailed, created_at, expires_at";

/// An invitation as stored, without its token.
#[derive(Debug, Clone)]
pub struct Invitation {
    pub id: String,
    /// Lower case.
    pub email: String,
    /// A role name as the API spells it, for example `org_admin`.
    pub role: String,
    pub organisation_id: String,
    /// Set for an invitation to one workspace.
    pub workspace_id: Option<String>,
    /// The principal who sent it.
    pub invited_by: String,
    /// The link went to the address by email.
    pub emailed: bool,
    pub created_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl Invitation {
    pub fn expired(&self) -> bool {
        self.expires_at <= Utc::now()
    }

    fn from_pg(r: &PgRow) -> Result<Self, sqlx::Error> {
        Ok(Self {
            id: r.try_get("id")?,
            email: r.try_get("email")?,
            role: r.try_get("role")?,
            organisation_id: r.try_get("organisation_id")?,
            workspace_id: r.try_get("workspace_id")?,
            invited_by: r.try_get("invited_by")?,
            emailed: r.try_get("emailed")?,
            created_at: r.try_get("created_at")?,
            expires_at: r.try_get("expires_at")?,
        })
    }
}

/// What a new or renewed invitation is made of.
pub struct NewInvitation<'a> {
    pub id: &'a str,
    pub token_hash: &'a [u8],
    pub email: &'a str,
    pub role: &'a str,
    pub organisation_id: &'a str,
    pub workspace_id: Option<&'a str>,
    pub invited_by: &'a str,
    pub expires_at: DateTime<Utc>,
}

impl Store {
    /// Stores an invitation. One for the same address and scope is replaced, keeping its id.
    pub async fn invitation_upsert(&self, n: &NewInvitation<'_>) -> Result<Invitation, StoreError> {
        let r = sqlx::query(&format!(
            "INSERT INTO invitations (id, token_hash, email, role, organisation_id, workspace_id, invited_by, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
             ON CONFLICT (email, organisation_id, (coalesce(workspace_id, ''))) DO UPDATE SET
                 token_hash = EXCLUDED.token_hash, role = EXCLUDED.role, invited_by = EXCLUDED.invited_by,
                 emailed = false, created_at = now(), expires_at = EXCLUDED.expires_at
             RETURNING {COLUMNS}"
        ))
        .bind(n.id)
        .bind(n.token_hash)
        .bind(n.email)
        .bind(n.role)
        .bind(n.organisation_id)
        .bind(n.workspace_id)
        .bind(n.invited_by)
        .bind(n.expires_at)
        .fetch_one(&self.pool)
        .await?;
        Ok(Invitation::from_pg(&r)?)
    }

    /// Gives an existing invitation a new token and lifetime.
    pub async fn invitation_renew(
        &self,
        id: &str,
        token_hash: &[u8],
        invited_by: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<Option<Invitation>, StoreError> {
        let r = sqlx::query(&format!(
            "UPDATE invitations SET token_hash = $2, invited_by = $3, emailed = false, created_at = now(), expires_at = $4
             WHERE id = $1 RETURNING {COLUMNS}"
        ))
        .bind(id)
        .bind(token_hash)
        .bind(invited_by)
        .bind(expires_at)
        .fetch_optional(&self.pool)
        .await?;
        Ok(r.as_ref().map(Invitation::from_pg).transpose()?)
    }

    /// Records that the link went out by email.
    pub async fn invitation_mark_emailed(&self, id: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE invitations SET emailed = true WHERE id = $1").bind(id).execute(&self.pool).await?;
        Ok(())
    }

    /// The invitations of an organisation, or of one of its workspaces, oldest first.
    pub async fn invitations_list(&self, org: &str, ws: Option<&str>) -> Result<Vec<Invitation>, StoreError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM invitations
             WHERE organisation_id = $1 AND ($2::text IS NULL OR workspace_id = $2) ORDER BY created_at, id"
        ))
        .bind(org)
        .bind(ws)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(Invitation::from_pg).collect::<Result<Vec<_>, _>>()?)
    }

    /// The invitations sent to an address that can still be accepted.
    pub async fn invitations_for_email(&self, email: &str) -> Result<Vec<Invitation>, StoreError> {
        let rows = sqlx::query(&format!(
            "SELECT {COLUMNS} FROM invitations WHERE email = $1 AND expires_at > now() ORDER BY created_at, id"
        ))
        .bind(email)
        .fetch_all(&self.pool)
        .await?;
        Ok(rows.iter().map(Invitation::from_pg).collect::<Result<Vec<_>, _>>()?)
    }

    pub async fn invitation_by_id(&self, id: &str) -> Result<Option<Invitation>, StoreError> {
        let r = sqlx::query(&format!("SELECT {COLUMNS} FROM invitations WHERE id = $1"))
            .bind(id)
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.as_ref().map(Invitation::from_pg).transpose()?)
    }

    /// The invitation a link carries, while it can be accepted.
    pub async fn invitation_by_token(&self, token_hash: &[u8]) -> Result<Option<Invitation>, StoreError> {
        let r = sqlx::query(&format!("SELECT {COLUMNS} FROM invitations WHERE token_hash = $1 AND expires_at > now()"))
            .bind(token_hash)
            .fetch_optional(&self.pool)
            .await?;
        Ok(r.as_ref().map(Invitation::from_pg).transpose()?)
    }

    /// Removes an invitation: accepted, declined or revoked. False when it was already gone.
    pub async fn invitation_delete(&self, id: &str) -> Result<bool, StoreError> {
        Ok(sqlx::query("DELETE FROM invitations WHERE id = $1").bind(id).execute(&self.pool).await?.rows_affected() > 0)
    }

    /// Removes invitations that expired a while ago.
    pub async fn invitations_prune(&self) -> Result<u64, StoreError> {
        Ok(sqlx::query("DELETE FROM invitations WHERE expires_at <= now() - make_interval(days => $1)")
            .bind(KEEP_EXPIRED_DAYS as i32)
            .execute(&self.pool)
            .await?
            .rows_affected())
    }
}
