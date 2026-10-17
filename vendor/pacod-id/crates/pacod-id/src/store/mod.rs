//! PostgreSQL. Runtime queries (`sqlx::query`) throughout; the only macro is
//! `migrate!`. pacod-id connects directly or through a pool in session mode: the feed uses
//! LISTEN.

pub mod accounts;
mod audit;
pub mod authz;
pub mod changes;
mod jobs;
mod keys;
mod mail;
mod misc;
pub mod oidc;
pub mod service;
mod throttle;

use std::time::Duration;

pub use accounts::{Account, NewAccount, normalize_email};
pub use audit::AuditEntry;
pub use keys::KeyRow;
pub use mail::MailRow;
pub use oidc::{Code, Flow, Grant, NewGrant, NewSession, Session};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Postgres, Transaction};
pub use throttle::Hit;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("not found")]
    NotFound,
    #[error("conflict: {0}")]
    Conflict(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error("stored data does not read: {0}")]
    Corrupt(String),
}

pub type Tx = Transaction<'static, Postgres>;

#[derive(Clone)]
pub struct Store {
    pool: PgPool,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Store")
    }
}

impl Store {
    pub async fn connect(url: &str) -> Result<Self, StoreError> {
        let options: PgConnectOptions = url.parse()?;
        let pool = PgPoolOptions::new()
            .max_connections(16)
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(options.application_name("pacod-id"))
            .await?;
        Ok(Store { pool })
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn migrate(&self) -> Result<(), StoreError> {
        sqlx::migrate!("./migrations").run(&self.pool).await.map_err(|e| StoreError::Corrupt(e.to_string()))
    }

    /// Whether this version has migrations the database lacks (or the database has none at all):
    /// tools that write must then wait for the service, or `pacod-id migrate`.
    pub async fn migrations_pending(&self) -> Result<bool, StoreError> {
        let applied: Vec<i64> = sqlx::query_scalar("SELECT version FROM _sqlx_migrations WHERE success")
            .fetch_all(&self.pool)
            .await
            .unwrap_or_default();
        Ok(sqlx::migrate!("./migrations").iter().any(|m| !applied.contains(&m.version)))
    }

    pub async fn begin(&self) -> Result<Tx, StoreError> {
        Ok(self.pool.begin().await?)
    }

    /// Whether the database answers.
    pub async fn ping(&self) -> bool {
        sqlx::query("SELECT 1").execute(&self.pool).await.is_ok()
    }
}
