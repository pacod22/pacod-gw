//! Signing keys.

use chrono::{DateTime, Utc};
use sqlx::Row;

use super::{Store, StoreError, Tx};
use crate::crypto::envelope::Sealed;

#[derive(Debug, Clone)]
pub struct KeyRow {
    pub kid: String,
    pub alg: String,
    pub state: String,
    pub public_jwk: serde_json::Value,
    pub private_enc: Sealed,
    pub created_at: DateTime<Utc>,
    pub activates_at: DateTime<Utc>,
    pub retires_at: Option<DateTime<Utc>>,
}

fn row_of(r: &sqlx::postgres::PgRow) -> Result<KeyRow, StoreError> {
    let private_enc: serde_json::Value = r.try_get("private_enc")?;
    Ok(KeyRow {
        kid: r.try_get("kid")?,
        alg: r.try_get("alg")?,
        state: r.try_get("state")?,
        public_jwk: r.try_get("public_jwk")?,
        private_enc: serde_json::from_value(private_enc).map_err(|e| StoreError::Corrupt(e.to_string()))?,
        created_at: r.try_get("created_at")?,
        activates_at: r.try_get("activates_at")?,
        retires_at: r.try_get("retires_at")?,
    })
}

impl Store {
    pub async fn keys(&self) -> Result<Vec<KeyRow>, StoreError> {
        let rows = sqlx::query("SELECT * FROM signing_keys ORDER BY created_at").fetch_all(self.pool()).await?;
        rows.iter().map(row_of).collect()
    }

    /// Serializes key changes between instances for one transaction.
    pub async fn lock_keys(tx: &mut Tx) -> Result<(), StoreError> {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtext('pacod-id signing keys'))").execute(&mut **tx).await?;
        Ok(())
    }

    pub async fn keys_in(tx: &mut Tx) -> Result<Vec<KeyRow>, StoreError> {
        let rows = sqlx::query("SELECT * FROM signing_keys ORDER BY created_at").fetch_all(&mut **tx).await?;
        rows.iter().map(row_of).collect()
    }

    pub async fn key_insert(tx: &mut Tx, k: &KeyRow) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO signing_keys (kid, alg, state, public_jwk, private_enc, activates_at, retires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&k.kid)
        .bind(&k.alg)
        .bind(&k.state)
        .bind(&k.public_jwk)
        .bind(serde_json::to_value(&k.private_enc).map_err(|e| StoreError::Corrupt(e.to_string()))?)
        .bind(k.activates_at)
        .bind(k.retires_at)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    pub async fn key_set_state(
        tx: &mut Tx,
        kid: &str,
        state: &str,
        retires_at: Option<DateTime<Utc>>,
    ) -> Result<(), StoreError> {
        sqlx::query("UPDATE signing_keys SET state = $2, retires_at = $3 WHERE kid = $1")
            .bind(kid)
            .bind(state)
            .bind(retires_at)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }

    pub async fn key_delete(tx: &mut Tx, kid: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM signing_keys WHERE kid = $1").bind(kid).execute(&mut **tx).await?;
        Ok(())
    }

    pub async fn key_set_private(tx: &mut Tx, kid: &str, sealed: &Sealed) -> Result<(), StoreError> {
        sqlx::query("UPDATE signing_keys SET private_enc = $2 WHERE kid = $1")
            .bind(kid)
            .bind(serde_json::to_value(sealed).map_err(|e| StoreError::Corrupt(e.to_string()))?)
            .execute(&mut **tx)
            .await?;
        Ok(())
    }
}
