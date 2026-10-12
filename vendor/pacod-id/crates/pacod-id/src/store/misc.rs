//! Instances and client secrets.

use chrono::{DateTime, Utc};
use sqlx::Row;

use super::{Store, StoreError};

/// A running instance, as its heartbeat left it.
#[derive(Debug, Clone)]
pub struct InstanceRow {
    pub id: String,
    pub kek_id: String,
    pub kek_check: String,
    pub seen_at: DateTime<Utc>,
}

impl Store {
    pub async fn instance_heartbeat(&self, id: &str, kek_id: &str, kek_check: &str) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO instances (id, kek_id, kek_check) VALUES ($1, $2, $3)
             ON CONFLICT (id) DO UPDATE SET kek_id = $2, kek_check = $3, seen_at = now()",
        )
        .bind(id)
        .bind(kek_id)
        .bind(kek_check)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Instances seen within `alive_seconds`.
    pub async fn instances_alive(&self, alive_seconds: u32) -> Result<Vec<InstanceRow>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, kek_id, kek_check, seen_at FROM instances WHERE seen_at > now() - make_interval(secs => $1)",
        )
        .bind(f64::from(alive_seconds))
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok(InstanceRow {
                    id: r.try_get("id")?,
                    kek_id: r.try_get("kek_id")?,
                    kek_check: r.try_get("kek_check")?,
                    seen_at: r.try_get("seen_at")?,
                })
            })
            .collect()
    }

    pub async fn instance_gone(&self, id: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM instances WHERE id = $1").bind(id).execute(self.pool()).await?;
        Ok(())
    }

    pub async fn instances_prune(&self, hours: u32) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM instances WHERE seen_at < now() - make_interval(hours => $1)")
            .bind(i32::try_from(hours).unwrap_or(i32::MAX))
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Records a client's secret (its HMAC), from the configuration or from `clients secret`.
    pub async fn client_set_secret(&self, client: &str, realm: &str, hmac: &[u8]) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO clients (id, realm, secret_hmac) VALUES ($1, $2, $3)
             ON CONFLICT (id) DO UPDATE SET realm = $2, secret_hmac = $3, updated_at = now()",
        )
        .bind(client)
        .bind(realm)
        .bind(hmac)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// The HMACs of every client secret, by client id.
    pub async fn client_secrets(&self) -> Result<Vec<(String, Vec<u8>)>, StoreError> {
        let rows = sqlx::query("SELECT id, secret_hmac FROM clients WHERE secret_hmac IS NOT NULL")
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(|r| Ok((r.try_get("id")?, r.try_get("secret_hmac")?))).collect()
    }
}
