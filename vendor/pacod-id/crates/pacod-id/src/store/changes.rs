//! The authorization change log behind the feed. Every change takes the one row of
//! authz_head, so versions are dense and in commit order: a follower that has seen version n has
//! seen everything before it. Transactions holding it stay short: no hashing, no mail.

use sqlx::{PgConnection, Row};

use super::StoreError;

/// Records a change in the caller's transaction and wakes the followers when it commits. Returns its version.
pub async fn record(
    conn: &mut PgConnection,
    kind: &str,
    product: Option<&str>,
    principal: Option<&str>,
    payload: serde_json::Value,
) -> Result<i64, StoreError> {
    let version: i64 = sqlx::query("UPDATE authz_head SET version = version + 1 RETURNING version")
        .fetch_one(&mut *conn)
        .await?
        .try_get("version")?;
    sqlx::query(
        "INSERT INTO authz_changes (version, kind, product, principal_id, payload) VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(version)
    .bind(kind)
    .bind(product)
    .bind(principal)
    .bind(payload)
    .execute(&mut *conn)
    .await?;
    sqlx::query("SELECT pg_notify('pid_authz', $1)").bind(version.to_string()).execute(&mut *conn).await?;
    Ok(version)
}

/// `session.revoked` for each (sid, account) pair.
pub async fn sessions_revoked(
    conn: &mut PgConnection,
    revoked: &[(String, String)],
    reason: &str,
) -> Result<(), StoreError> {
    for (sid, account) in revoked {
        record(
            conn,
            "session.revoked",
            None,
            Some(account),
            serde_json::json!({ "sid": sid, "sub": account, "reason": reason }),
        )
        .await?;
    }
    Ok(())
}
