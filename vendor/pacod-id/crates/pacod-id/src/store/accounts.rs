//! Accounts, their one-time links, and the browsers they signed in on.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::postgres::PgRow;

use super::{Store, StoreError, Tx};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub id: String,
    pub email: String,
    pub email_verified_at: Option<DateTime<Utc>>,
    pub email_verified_source: String,
    pub display_name: String,
    pub locale: String,
    pub status: String,
    pub password_hash: Option<String>,
    pub password_change_due: bool,
    pub signup_realm: String,
    pub last_realm: String,
    pub created_at: DateTime<Utc>,
    pub last_login_at: Option<DateTime<Utc>>,
}

impl Account {
    pub fn is_active(&self) -> bool {
        self.status == "active"
    }

    pub fn is_verified(&self) -> bool {
        self.email_verified_at.is_some()
    }
}

fn account_of(r: &PgRow) -> Result<Account, StoreError> {
    Ok(Account {
        id: r.try_get("id")?,
        email: r.try_get("email")?,
        email_verified_at: r.try_get("email_verified_at")?,
        email_verified_source: r.try_get("email_verified_source")?,
        display_name: r.try_get("display_name")?,
        locale: r.try_get("locale")?,
        status: r.try_get("status")?,
        password_hash: r.try_get("password_hash")?,
        password_change_due: r.try_get("password_change_due")?,
        signup_realm: r.try_get("signup_realm")?,
        last_realm: r.try_get("last_realm")?,
        created_at: r.try_get("created_at")?,
        last_login_at: r.try_get("last_login_at")?,
    })
}

/// What a new account starts with.
#[derive(Debug, Clone, Default)]
pub struct NewAccount {
    pub id: String,
    pub email: String,
    pub display_name: String,
    pub locale: String,
    pub password_hash: Option<String>,
    /// The address is proven already, and how: `verified`, `admin`, `federated`, `invitation`.
    pub verified_by: Option<String>,
    pub signup_realm: String,
}

/// A one-time link, as stored.
#[derive(Debug, Clone)]
pub struct AccountToken {
    pub account_id: String,
    pub email: String,
    pub realm: String,
}

/// Addresses are compared lowercased and without surrounding space.
pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

impl Store {
    pub async fn account(&self, id: &str) -> Result<Option<Account>, StoreError> {
        let r = sqlx::query("SELECT * FROM accounts WHERE id = $1").bind(id).fetch_optional(self.pool()).await?;
        r.as_ref().map(account_of).transpose()
    }

    pub async fn account_by_email(&self, email: &str) -> Result<Option<Account>, StoreError> {
        let r = sqlx::query("SELECT * FROM accounts WHERE email = $1")
            .bind(normalize_email(email))
            .fetch_optional(self.pool())
            .await?;
        r.as_ref().map(account_of).transpose()
    }

    /// Creates an account; Conflict when the address is taken.
    pub async fn account_create(&self, a: &NewAccount) -> Result<Account, StoreError> {
        account_insert(&mut *self.pool().acquire().await?, a).await
    }

    /// Sets a new password hash; clears a due change.
    pub async fn account_set_password(&self, id: &str, hash: &str) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE accounts SET password_hash = $2, password_changed_at = now(), password_change_due = false,
               updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(hash)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Replaces a hash after a sign-in proved the password (a bcrypt or weaker one).
    pub async fn account_rehash(&self, id: &str, hash: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE accounts SET password_hash = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(hash)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn account_set_change_due(&self, id: &str, due: bool) -> Result<(), StoreError> {
        sqlx::query("UPDATE accounts SET password_change_due = $2, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(due)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Marks the address proven, unless it already is.
    pub async fn account_mark_verified(&self, id: &str, source: &str) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE accounts SET email_verified_at = now(), email_verified_source = $2, updated_at = now()
             WHERE id = $1 AND email_verified_at IS NULL",
        )
        .bind(id)
        .bind(source)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn account_signed_in(&self, id: &str, realm: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE accounts SET last_login_at = now(), last_realm = $2 WHERE id = $1")
            .bind(id)
            .bind(realm)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Whether the account holds any stored role (implicit roles are never stored).
    pub async fn account_holds_roles(&self, id: &str) -> Result<bool, StoreError> {
        let r = sqlx::query("SELECT EXISTS (SELECT 1 FROM role_bindings WHERE principal_id = $1) AS held")
            .bind(id)
            .fetch_one(self.pool())
            .await?;
        Ok(r.try_get("held")?)
    }

    // One-time links -------------------------------------------------------------------------

    /// Stores a link's hash. The account's earlier links of the same purpose go: only the
    /// newest one works.
    pub async fn account_token_create(
        &self,
        hash: &[u8],
        purpose: &str,
        account: &str,
        email: &str,
        realm: &str,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        let mut tx = self.begin().await?;
        sqlx::query("DELETE FROM account_tokens WHERE account_id = $1 AND purpose = $2")
            .bind(account)
            .bind(purpose)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO account_tokens (token_hash, purpose, account_id, email, realm, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(hash)
        .bind(purpose)
        .bind(account)
        .bind(email)
        .bind(realm)
        .bind(expires_at)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Takes a link: it works once. None when unknown, used or expired.
    pub async fn account_token_take(&self, hash: &[u8], purpose: &str) -> Result<Option<AccountToken>, StoreError> {
        let r = sqlx::query(
            "DELETE FROM account_tokens WHERE token_hash = $1 AND purpose = $2 RETURNING account_id, email, realm, expires_at",
        )
        .bind(hash)
        .bind(purpose)
        .fetch_optional(self.pool())
        .await?;
        let Some(r) = r else { return Ok(None) };
        let expires_at: DateTime<Utc> = r.try_get("expires_at")?;
        if expires_at < Utc::now() {
            return Ok(None);
        }
        Ok(Some(AccountToken {
            account_id: r.try_get("account_id")?,
            email: r.try_get("email")?,
            realm: r.try_get("realm")?,
        }))
    }

    pub async fn account_tokens_delete(&self, account: &str, purpose: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM account_tokens WHERE account_id = $1 AND purpose = $2")
            .bind(account)
            .bind(purpose)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    // Known devices --------------------------------------------------------------------------

    pub async fn device_known(&self, device: &[u8], account: &str) -> Result<bool, StoreError> {
        let r =
            sqlx::query("SELECT EXISTS (SELECT 1 FROM known_devices WHERE device_hash = $1 AND account_id = $2) AS k")
                .bind(device)
                .bind(account)
                .fetch_one(self.pool())
                .await?;
        Ok(r.try_get("k")?)
    }

    /// Remembers a browser for an account. Returns whether the account had other browsers
    /// before (then a new one is worth a mail).
    pub async fn device_remember(&self, device: &[u8], account: &str) -> Result<bool, StoreError> {
        let others =
            sqlx::query("SELECT EXISTS (SELECT 1 FROM known_devices WHERE account_id = $2 AND device_hash <> $1) AS o")
                .bind(device)
                .bind(account)
                .fetch_one(self.pool())
                .await?
                .try_get("o")?;
        sqlx::query(
            "INSERT INTO known_devices (device_hash, account_id) VALUES ($1, $2)
             ON CONFLICT (device_hash, account_id) DO UPDATE SET last_seen_at = now()",
        )
        .bind(device)
        .bind(account)
        .execute(self.pool())
        .await?;
        Ok(others)
    }

    /// Whether `tx` sees the account (for writes that must not outlive a deletion).
    pub async fn account_exists_in(tx: &mut Tx, id: &str) -> Result<bool, StoreError> {
        let r = sqlx::query("SELECT EXISTS (SELECT 1 FROM accounts WHERE id = $1) AS e")
            .bind(id)
            .fetch_one(&mut **tx)
            .await?;
        Ok(r.try_get("e")?)
    }
}

impl Store {
    /// A link, without using it up (to check a new password before the reset link goes).
    pub async fn account_token_peek(&self, hash: &[u8], purpose: &str) -> Result<Option<AccountToken>, StoreError> {
        let r = sqlx::query(
            "SELECT account_id, email, realm FROM account_tokens WHERE token_hash = $1 AND purpose = $2 AND expires_at > now()",
        )
        .bind(hash)
        .bind(purpose)
        .fetch_optional(self.pool())
        .await?;
        r.map(|r| {
            Ok(AccountToken {
                account_id: r.try_get("account_id")?,
                email: r.try_get("email")?,
                realm: r.try_get("realm")?,
            })
        })
        .transpose()
    }

    /// Records a consent to a realm's document.
    pub async fn consent(&self, account: &str, realm: &str, document: &str, version: &str) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO consents (account_id, realm, document, version) VALUES ($1, $2, $3, $4)
             ON CONFLICT (account_id, realm, document) DO UPDATE SET version = $4, accepted_at = now()",
        )
        .bind(account)
        .bind(realm)
        .bind(document)
        .bind(version)
        .execute(self.pool())
        .await?;
        Ok(())
    }
}

/// Creates an account in the caller's transaction; `Conflict` when the address has one.
pub async fn account_insert(conn: &mut sqlx::PgConnection, a: &NewAccount) -> Result<Account, StoreError> {
    let r = sqlx::query(
        "INSERT INTO accounts (id, email, display_name, locale, password_hash, password_changed_at,
           email_verified_at, email_verified_source, signup_realm)
         VALUES ($1, $2, $3, $4, $5, CASE WHEN $5 IS NULL THEN NULL ELSE now() END,
           CASE WHEN $6::text IS NULL THEN NULL ELSE now() END, coalesce($6, ''), $7)
         ON CONFLICT (email) DO NOTHING RETURNING *",
    )
    .bind(&a.id)
    .bind(normalize_email(&a.email))
    .bind(&a.display_name)
    .bind(&a.locale)
    .bind(&a.password_hash)
    .bind(&a.verified_by)
    .bind(&a.signup_realm)
    .fetch_optional(conn)
    .await?;
    match r {
        Some(r) => account_of(&r),
        None => Err(StoreError::Conflict("an account with this address exists".into())),
    }
}

impl Store {
    /// A page of accounts, by address, after `after` (an address); only `realm`'s sign-ups when
    /// set.
    pub async fn accounts_list(
        &self,
        query: &str,
        realm: Option<&str>,
        after: &str,
        limit: i64,
    ) -> Result<Vec<Account>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM accounts
             WHERE ($1 = '' OR email ILIKE '%' || $1 || '%' OR display_name ILIKE '%' || $1 || '%')
               AND ($2::text IS NULL OR signup_realm = $2) AND email > $3
             ORDER BY email LIMIT $4",
        )
        .bind(query.replace(['%', '_'], ""))
        .bind(realm)
        .bind(after)
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(account_of).collect()
    }

    pub async fn account_set_profile(&self, id: &str, display_name: &str, locale: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE accounts SET display_name = $2, locale = $3, updated_at = now() WHERE id = $1")
            .bind(id)
            .bind(display_name)
            .bind(locale)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// A new address; proven (`verified_by`) or not. `Conflict` when another account has it.
    pub async fn account_set_email(&self, id: &str, email: &str, verified_by: Option<&str>) -> Result<(), StoreError> {
        let r = sqlx::query(
            "UPDATE accounts SET email = $2, email_verified_at = CASE WHEN $3::text IS NULL THEN NULL ELSE now() END,
               email_verified_source = coalesce($3, ''), updated_at = now() WHERE id = $1",
        )
        .bind(id)
        .bind(normalize_email(email))
        .bind(verified_by)
        .execute(self.pool())
        .await;
        match r {
            Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
                Err(StoreError::Conflict("another account has this address".into()))
            }
            r => {
                r?;
                Ok(())
            }
        }
    }
}

/// Sets an account's status in the caller's transaction.
pub async fn account_set_status(conn: &mut sqlx::PgConnection, id: &str, status: &str) -> Result<(), StoreError> {
    sqlx::query("UPDATE accounts SET status = $2, updated_at = now() WHERE id = $1")
        .bind(id)
        .bind(status)
        .execute(conn)
        .await?;
    Ok(())
}
