//! Service accounts and their tokens.

use chrono::{DateTime, Utc};
use sqlx::Row;

use super::{Store, StoreError};

/// A service token that verified, with its account.
#[derive(Debug, Clone)]
pub struct ServiceToken {
    pub token_id: String,
    pub service_account_id: String,
    pub service_account_name: String,
    pub organisation_id: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
}

impl Store {
    /// The active token with this HMAC (a pidt_ token under PID_TOKEN_PEPPER, or an imported one
    /// under its product's pepper), of an active service account.
    pub async fn service_token(
        &self,
        hmac: &[u8],
        legacy_product: Option<&str>,
    ) -> Result<Option<ServiceToken>, StoreError> {
        let r = sqlx::query(
            "SELECT t.id, t.service_account_id, t.expires_at, a.name, a.organisation_id
             FROM service_tokens t JOIN service_accounts a ON a.id = t.service_account_id
             WHERE t.status = 'active' AND a.status = 'active' AND (t.expires_at IS NULL OR t.expires_at > now())
               AND CASE WHEN $2::text IS NULL THEN t.token_hmac = $1 ELSE t.legacy_hmac = $1 AND t.legacy_pepper_id = $2 END",
        )
        .bind(hmac)
        .bind(legacy_product)
        .fetch_optional(self.pool())
        .await?;
        let Some(r) = r else { return Ok(None) };
        let token = ServiceToken {
            token_id: r.try_get("id")?,
            service_account_id: r.try_get("service_account_id")?,
            service_account_name: r.try_get("name")?,
            organisation_id: r.try_get("organisation_id")?,
            expires_at: r.try_get("expires_at")?,
        };
        sqlx::query("UPDATE service_tokens SET last_used_at = now() WHERE id = $1")
            .bind(&token.token_id)
            .execute(self.pool())
            .await?;
        Ok(Some(token))
    }

    /// A service account (tests and the admin API create them).
    pub async fn service_account_create(
        &self,
        id: &str,
        name: &str,
        organisation: Option<&str>,
        by: &str,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO service_accounts (id, name, organisation_id, created_by) VALUES ($1, $2, $3, $4)")
            .bind(id)
            .bind(name)
            .bind(organisation)
            .bind(by)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// A token of a service account: its HMAC (pidt_), or an imported product token's.
    pub async fn service_token_create(
        &self,
        id: &str,
        account: &str,
        hmac: &[u8],
        legacy_product: Option<&str>,
        hint: &str,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<(), StoreError> {
        let (token_hmac, legacy_hmac) = if legacy_product.is_some() { (None, Some(hmac)) } else { (Some(hmac), None) };
        sqlx::query(
            "INSERT INTO service_tokens (id, service_account_id, token_hmac, legacy_hmac, legacy_pepper_id, hint, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(id)
        .bind(account)
        .bind(token_hmac)
        .bind(legacy_hmac)
        .bind(legacy_product)
        .bind(hint)
        .bind(expires_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

/// A service account as stored.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ServiceAccount {
    pub id: String,
    pub name: String,
    pub organisation_id: Option<String>,
    pub status: String,
    pub created_by: String,
    pub created_at: DateTime<Utc>,
}

/// A service token as listed: never its secret.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ServiceTokenRow {
    pub id: String,
    pub service_account_id: String,
    pub hint: String,
    /// Imported from a product (`gw` for pgwa_ tokens).
    pub legacy_product: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub last_used_at: Option<DateTime<Utc>>,
    pub status: String,
    pub created_at: DateTime<Utc>,
}

fn service_account_of(r: &sqlx::postgres::PgRow) -> Result<ServiceAccount, StoreError> {
    Ok(ServiceAccount {
        id: r.try_get("id")?,
        name: r.try_get("name")?,
        organisation_id: r.try_get("organisation_id")?,
        status: r.try_get("status")?,
        created_by: r.try_get("created_by")?,
        created_at: r.try_get("created_at")?,
    })
}

fn token_of(r: &sqlx::postgres::PgRow) -> Result<ServiceTokenRow, StoreError> {
    Ok(ServiceTokenRow {
        id: r.try_get("id")?,
        service_account_id: r.try_get("service_account_id")?,
        hint: r.try_get("hint")?,
        legacy_product: r.try_get("legacy_pepper_id")?,
        expires_at: r.try_get("expires_at")?,
        last_used_at: r.try_get("last_used_at")?,
        status: r.try_get("status")?,
        created_at: r.try_get("created_at")?,
    })
}

impl Store {
    pub async fn service_account(&self, id: &str) -> Result<Option<ServiceAccount>, StoreError> {
        let r =
            sqlx::query("SELECT * FROM service_accounts WHERE id = $1").bind(id).fetch_optional(self.pool()).await?;
        r.as_ref().map(service_account_of).transpose()
    }

    /// The service accounts of an organisation, or of the platform (`None`).
    pub async fn service_accounts_of(&self, organisation: Option<&str>) -> Result<Vec<ServiceAccount>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM service_accounts WHERE organisation_id IS NOT DISTINCT FROM $1 ORDER BY created_at, id",
        )
        .bind(organisation)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(service_account_of).collect()
    }

    pub async fn service_account_rename(&self, id: &str, name: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE service_accounts SET name = $2 WHERE id = $1")
            .bind(id)
            .bind(name)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn service_tokens_of(&self, account: &str) -> Result<Vec<ServiceTokenRow>, StoreError> {
        let rows = sqlx::query("SELECT * FROM service_tokens WHERE service_account_id = $1 ORDER BY created_at, id")
            .bind(account)
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(token_of).collect()
    }

    pub async fn service_token_row(&self, id: &str) -> Result<Option<ServiceTokenRow>, StoreError> {
        let r = sqlx::query("SELECT * FROM service_tokens WHERE id = $1").bind(id).fetch_optional(self.pool()).await?;
        r.as_ref().map(token_of).transpose()
    }
}

pub async fn service_account_set_status(
    conn: &mut sqlx::PgConnection,
    id: &str,
    status: &str,
) -> Result<(), StoreError> {
    sqlx::query("UPDATE service_accounts SET status = $2 WHERE id = $1").bind(id).bind(status).execute(conn).await?;
    Ok(())
}

/// Revokes tokens (all of an account's, or one); the ids that were active.
pub async fn service_tokens_revoke(
    conn: &mut sqlx::PgConnection,
    account: &str,
    token: Option<&str>,
) -> Result<Vec<String>, StoreError> {
    let rows = sqlx::query(
        "UPDATE service_tokens SET status = 'revoked' WHERE service_account_id = $1 AND ($2::text IS NULL OR id = $2)
           AND status = 'active' RETURNING id",
    )
    .bind(account)
    .bind(token)
    .fetch_all(conn)
    .await?;
    rows.iter().map(|r| Ok(r.try_get("id")?)).collect()
}

pub async fn service_account_delete(conn: &mut sqlx::PgConnection, id: &str) -> Result<(), StoreError> {
    sqlx::query("DELETE FROM service_accounts WHERE id = $1").bind(id).execute(conn).await?;
    Ok(())
}
