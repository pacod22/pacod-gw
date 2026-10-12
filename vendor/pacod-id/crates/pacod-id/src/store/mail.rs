//! The mail outbox.

use sqlx::Row;

use super::{Store, StoreError};

#[derive(Debug, Clone)]
pub struct MailRow {
    pub id: i64,
    pub template: String,
    pub realm: String,
    pub to_address: String,
    pub params: serde_json::Value,
}

/// How many attempts a mail gets: with the backoff below (doubling, at most four hours apart),
/// several hours; a reset link of an hour may then arrive expired.
const MAX_ATTEMPTS: i32 = 10;

impl Store {
    pub async fn mail_enqueue(
        &self,
        template: &str,
        realm: &str,
        to: &str,
        params: &serde_json::Value,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO mail_outbox (template, realm, to_address, params) VALUES ($1, $2, $3, $4)")
            .bind(template)
            .bind(realm)
            .bind(to)
            .bind(params)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Mails due for sending, claimed for a minute so that a parallel sender skips them.
    pub async fn mail_due(&self, limit: i64) -> Result<Vec<MailRow>, StoreError> {
        let rows = sqlx::query(
            "UPDATE mail_outbox SET next_attempt_at = now() + interval '1 minute'
             WHERE id IN (SELECT id FROM mail_outbox WHERE sent_at IS NULL AND next_attempt_at <= now()
                          AND attempts < $2 ORDER BY id LIMIT $1 FOR UPDATE SKIP LOCKED)
             RETURNING id, template, realm, to_address, params",
        )
        .bind(limit)
        .bind(MAX_ATTEMPTS)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok(MailRow {
                    id: r.try_get("id")?,
                    template: r.try_get("template")?,
                    realm: r.try_get("realm")?,
                    to_address: r.try_get("to_address")?,
                    params: r.try_get("params")?,
                })
            })
            .collect()
    }

    /// Marks a mail sent and blanks its parameters: the links in them are not kept.
    pub async fn mail_sent(&self, id: i64) -> Result<(), StoreError> {
        sqlx::query("UPDATE mail_outbox SET sent_at = now(), params = '{}', error = '' WHERE id = $1")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Records a failed attempt; the next one waits twice as long, from a minute up to 4 hours.
    pub async fn mail_failed(&self, id: i64, error: &str) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE mail_outbox SET attempts = attempts + 1, error = $2,
               next_attempt_at = now() + least(interval '4 hours', interval '1 minute' * power(2, attempts))
             WHERE id = $1",
        )
        .bind(id)
        .bind(error.chars().take(500).collect::<String>())
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Deletes sent mails after a day and abandoned ones after a week; blanks the parameters of
    /// abandoned ones at once.
    pub async fn mail_prune(&self) -> Result<(), StoreError> {
        sqlx::query("UPDATE mail_outbox SET params = '{}' WHERE sent_at IS NULL AND attempts >= $1 AND params <> '{}'")
            .bind(MAX_ATTEMPTS)
            .execute(self.pool())
            .await?;
        sqlx::query(
            "DELETE FROM mail_outbox WHERE (sent_at IS NOT NULL AND sent_at < now() - interval '1 day')
             OR (sent_at IS NULL AND created_at < now() - interval '7 days')",
        )
        .execute(self.pool())
        .await?;
        Ok(())
    }
}
