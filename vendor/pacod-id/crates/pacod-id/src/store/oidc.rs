//! Sign-in flows, authorization codes, single sign-on sessions and grants.

use chrono::{DateTime, Utc};
use sqlx::Row;
use sqlx::postgres::PgRow;

use super::{Store, StoreError, Tx};

#[derive(Debug, Clone)]
pub struct Flow {
    pub id: String,
    pub realm: String,
    pub client_id: String,
    pub request: serde_json::Value,
    pub browser_hash: Vec<u8>,
    pub account_id: Option<String>,
    pub stage: String,
    pub expires_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct Code {
    pub account_id: String,
    pub sso_session_id: Option<String>,
    pub client_id: String,
    pub redirect_uri: String,
    pub pkce_challenge: String,
    pub nonce: Option<String>,
    pub scope: String,
    pub auth_time: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    /// Set when the code had been used already: then this take is a replay.
    pub used_before: bool,
}

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub account_id: String,
    pub realm: String,
    pub remember_me: bool,
    pub auth_time: DateTime<Utc>,
    pub amr: Vec<String>,
    pub acr: String,
    pub ip: String,
    pub user_agent: String,
    pub device_label: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl Session {
    pub fn is_live(&self) -> bool {
        self.revoked_at.is_none() && self.expires_at > Utc::now()
    }
}

#[derive(Debug, Clone)]
pub struct Grant {
    pub id: String,
    pub sso_session_id: Option<String>,
    pub sid: String,
    pub client_id: String,
    pub account_id: String,
    pub scope: String,
    pub refresh_hash: Vec<u8>,
    pub previous_hash: Option<Vec<u8>>,
    pub successor_enc: Option<serde_json::Value>,
    pub successor_until: Option<DateTime<Utc>>,
    pub act_sub: Option<String>,
    pub auth_time: DateTime<Utc>,
    pub amr: Vec<String>,
    pub idle_expires_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
    pub legacy_clients: Option<Vec<String>>,
}

fn session_of(r: &PgRow) -> Result<Session, StoreError> {
    Ok(Session {
        id: r.try_get("id")?,
        account_id: r.try_get("account_id")?,
        realm: r.try_get("realm")?,
        remember_me: r.try_get("remember_me")?,
        auth_time: r.try_get("auth_time")?,
        amr: r.try_get("amr")?,
        acr: r.try_get("acr")?,
        ip: r.try_get("ip")?,
        user_agent: r.try_get("user_agent")?,
        device_label: r.try_get("device_label")?,
        created_at: r.try_get("created_at")?,
        last_seen_at: r.try_get("last_seen_at")?,
        expires_at: r.try_get("expires_at")?,
        revoked_at: r.try_get("revoked_at")?,
    })
}

fn grant_of(r: &PgRow) -> Result<Grant, StoreError> {
    Ok(Grant {
        id: r.try_get("id")?,
        sso_session_id: r.try_get("sso_session_id")?,
        sid: r.try_get("sid")?,
        client_id: r.try_get("client_id")?,
        account_id: r.try_get("account_id")?,
        scope: r.try_get("scope")?,
        refresh_hash: r.try_get("refresh_hash")?,
        previous_hash: r.try_get("previous_hash")?,
        successor_enc: r.try_get("successor_enc")?,
        successor_until: r.try_get("successor_until")?,
        act_sub: r.try_get("act_sub")?,
        auth_time: r.try_get("auth_time")?,
        amr: r.try_get("amr")?,
        idle_expires_at: r.try_get("idle_expires_at")?,
        expires_at: r.try_get("expires_at")?,
        revoked_at: r.try_get("revoked_at")?,
        legacy_clients: r.try_get("legacy_clients")?,
    })
}

/// A new SSO session.
#[derive(Debug, Clone)]
pub struct NewSession<'a> {
    pub id: &'a str,
    pub account_id: &'a str,
    pub realm: &'a str,
    pub cookie_hash: &'a [u8],
    pub remember_me: bool,
    pub amr: &'a [String],
    pub ip: &'a str,
    pub user_agent: &'a str,
    pub device_label: &'a str,
    pub expires_at: DateTime<Utc>,
}

/// A new grant.
#[derive(Debug, Clone)]
pub struct NewGrant<'a> {
    pub id: &'a str,
    pub sso_session_id: Option<&'a str>,
    pub sid: &'a str,
    pub client_id: &'a str,
    pub account_id: &'a str,
    pub scope: &'a str,
    pub refresh_hash: &'a [u8],
    pub act_sub: Option<&'a str>,
    pub auth_time: DateTime<Utc>,
    pub amr: &'a [String],
    pub idle_expires_at: Option<DateTime<Utc>>,
    pub expires_at: DateTime<Utc>,
    pub code_hash: Option<&'a [u8]>,
}

impl Store {
    // Flows ----------------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn flow_create(
        &self,
        id: &str,
        realm: &str,
        client: &str,
        request: &serde_json::Value,
        browser_hash: &[u8],
        stage: &str,
        account: Option<&str>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO auth_flows (id, realm, client_id, request, browser_hash, stage, account_id, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        )
        .bind(id)
        .bind(realm)
        .bind(client)
        .bind(request)
        .bind(browser_hash)
        .bind(stage)
        .bind(account)
        .bind(expires_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn flow(&self, id: &str) -> Result<Option<Flow>, StoreError> {
        let r = sqlx::query("SELECT * FROM auth_flows WHERE id = $1 AND expires_at > now()")
            .bind(id)
            .fetch_optional(self.pool())
            .await?;
        r.map(|r| {
            Ok(Flow {
                id: r.try_get("id")?,
                realm: r.try_get("realm")?,
                client_id: r.try_get("client_id")?,
                request: r.try_get("request")?,
                browser_hash: r.try_get("browser_hash")?,
                account_id: r.try_get("account_id")?,
                stage: r.try_get("stage")?,
                expires_at: r.try_get("expires_at")?,
            })
        })
        .transpose()
    }

    pub async fn flow_set_stage(&self, id: &str, stage: &str, account: Option<&str>) -> Result<(), StoreError> {
        sqlx::query("UPDATE auth_flows SET stage = $2, account_id = coalesce($3, account_id) WHERE id = $1")
            .bind(id)
            .bind(stage)
            .bind(account)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    pub async fn flow_set_request(
        &self,
        id: &str,
        request: &serde_json::Value,
        stage: &str,
        account: Option<&str>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "UPDATE auth_flows SET request = $2, stage = $3, account_id = coalesce($4, account_id) WHERE id = $1",
        )
        .bind(id)
        .bind(request)
        .bind(stage)
        .bind(account)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn flow_delete(&self, id: &str) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM auth_flows WHERE id = $1").bind(id).execute(self.pool()).await?;
        Ok(())
    }

    // Codes ----------------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    pub async fn code_create(
        &self,
        hash: &[u8],
        account: &str,
        session: Option<&str>,
        client: &str,
        redirect_uri: &str,
        challenge: &str,
        nonce: Option<&str>,
        scope: &str,
        auth_time: DateTime<Utc>,
        expires_at: DateTime<Utc>,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO auth_codes (code_hash, account_id, sso_session_id, client_id, redirect_uri, pkce_challenge,
               nonce, scope, auth_time, expires_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
        )
        .bind(hash)
        .bind(account)
        .bind(session)
        .bind(client)
        .bind(redirect_uri)
        .bind(challenge)
        .bind(nonce)
        .bind(scope)
        .bind(auth_time)
        .bind(expires_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// Takes a code. A code can be taken once; a second take is reported (`used_before`).
    /// Takes a code of `client`: another client's code is not touched, so it neither spends
    /// the code nor makes the rightful redemption look like a replay.
    pub async fn code_take(&self, hash: &[u8], client: &str) -> Result<Option<Code>, StoreError> {
        let r = sqlx::query(
            "UPDATE auth_codes SET used_at = coalesce(used_at, now()) WHERE code_hash = $1 AND client_id = $2
             RETURNING account_id, sso_session_id, client_id, redirect_uri, pkce_challenge, nonce, scope, auth_time,
               expires_at, (used_at <> now()) AS used_before",
        )
        .bind(hash)
        .bind(client)
        .fetch_optional(self.pool())
        .await?;
        r.map(|r| {
            Ok(Code {
                account_id: r.try_get("account_id")?,
                sso_session_id: r.try_get("sso_session_id")?,
                client_id: r.try_get("client_id")?,
                redirect_uri: r.try_get("redirect_uri")?,
                pkce_challenge: r.try_get("pkce_challenge")?,
                nonce: r.try_get("nonce")?,
                scope: r.try_get("scope")?,
                auth_time: r.try_get("auth_time")?,
                expires_at: r.try_get("expires_at")?,
                used_before: r.try_get("used_before")?,
            })
        })
        .transpose()
    }

    // Sessions -------------------------------------------------------------------------------

    pub async fn session_create(&self, s: &NewSession<'_>) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO sso_sessions (id, account_id, realm, cookie_hash, remember_me, auth_time, amr, ip, user_agent,
               device_label, expires_at) VALUES ($1, $2, $3, $4, $5, now(), $6, $7, $8, $9, $10)",
        )
        .bind(s.id)
        .bind(s.account_id)
        .bind(s.realm)
        .bind(s.cookie_hash)
        .bind(s.remember_me)
        .bind(s.amr)
        .bind(s.ip)
        .bind(s.user_agent)
        .bind(s.device_label)
        .bind(s.expires_at)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn session_by_cookie(&self, hash: &[u8]) -> Result<Option<Session>, StoreError> {
        let r = sqlx::query("SELECT * FROM sso_sessions WHERE cookie_hash = $1")
            .bind(hash)
            .fetch_optional(self.pool())
            .await?;
        r.as_ref().map(session_of).transpose()
    }

    pub async fn session(&self, id: &str) -> Result<Option<Session>, StoreError> {
        let r = sqlx::query("SELECT * FROM sso_sessions WHERE id = $1").bind(id).fetch_optional(self.pool()).await?;
        r.as_ref().map(session_of).transpose()
    }

    pub async fn session_touch(&self, id: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE sso_sessions SET last_seen_at = now() WHERE id = $1").bind(id).execute(self.pool()).await?;
        Ok(())
    }

    /// The live sessions of an account.
    pub async fn sessions_of(&self, account: &str) -> Result<Vec<Session>, StoreError> {
        let rows = sqlx::query(
            "SELECT * FROM sso_sessions WHERE account_id = $1 AND revoked_at IS NULL AND expires_at > now()
             ORDER BY last_seen_at DESC",
        )
        .bind(account)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(session_of).collect()
    }

    /// Revokes sessions and their grants in `tx`; returns the (sid, account) pairs revoked.
    /// `which` is the WHERE clause on sso_sessions with $1 as its one parameter.
    async fn sessions_revoke_where(
        tx: &mut Tx,
        which: &str,
        arg: &str,
        reason: &str,
    ) -> Result<Vec<(String, String)>, StoreError> {
        let rows = sqlx::query(&format!(
            "UPDATE sso_sessions SET revoked_at = now(), revoked_reason = $2 WHERE {which} AND revoked_at IS NULL
             RETURNING id, account_id"
        ))
        .bind(arg)
        .bind(reason)
        .fetch_all(&mut **tx)
        .await?;
        let ids: Vec<(String, String)> =
            rows.iter().map(|r| Ok((r.try_get("id")?, r.try_get("account_id")?))).collect::<Result<_, StoreError>>()?;
        let sids: Vec<&str> = ids.iter().map(|(s, _)| s.as_str()).collect();
        sqlx::query("UPDATE grants SET revoked_at = now() WHERE sid = ANY($1) AND revoked_at IS NULL")
            .bind(&sids)
            .execute(&mut **tx)
            .await?;
        Ok(ids)
    }

    pub async fn session_revoke(tx: &mut Tx, sid: &str, reason: &str) -> Result<Vec<(String, String)>, StoreError> {
        Self::sessions_revoke_where(tx, "id = $1", sid, reason).await
    }

    /// Every session of an account, and every grant (also those without a session).
    pub async fn sessions_revoke_all(
        tx: &mut Tx,
        account: &str,
        reason: &str,
    ) -> Result<Vec<(String, String)>, StoreError> {
        let mut out = Self::sessions_revoke_where(tx, "account_id = $1", account, reason).await?;
        let rows = sqlx::query(
            "UPDATE grants SET revoked_at = now() WHERE account_id = $1 AND revoked_at IS NULL RETURNING sid",
        )
        .bind(account)
        .fetch_all(&mut **tx)
        .await?;
        for r in rows {
            let sid: String = r.try_get("sid")?;
            if !out.iter().any(|(s, _)| *s == sid) {
                out.push((sid, account.to_owned()));
            }
        }
        Ok(out)
    }

    /// The account's other sessions and their grants, and its grants without a session (apps):
    /// what "sign out everywhere else" ends. `keep` stays.
    pub async fn sessions_revoke_others(
        tx: &mut Tx,
        account: &str,
        keep: &str,
        reason: &str,
    ) -> Result<Vec<(String, String)>, StoreError> {
        let rows = sqlx::query(
            "UPDATE sso_sessions SET revoked_at = now(), revoked_reason = $3
             WHERE account_id = $1 AND id <> $2 AND revoked_at IS NULL RETURNING id",
        )
        .bind(account)
        .bind(keep)
        .bind(reason)
        .fetch_all(&mut **tx)
        .await?;
        let mut out: Vec<(String, String)> =
            rows.iter().map(|r| Ok((r.try_get("id")?, account.to_owned()))).collect::<Result<_, StoreError>>()?;
        let grants = sqlx::query(
            "UPDATE grants SET revoked_at = now() WHERE account_id = $1 AND sid IS DISTINCT FROM $2 AND revoked_at IS NULL
             RETURNING sid",
        )
        .bind(account)
        .bind(keep)
        .fetch_all(&mut **tx)
        .await?;
        for r in grants {
            let sid: Option<String> = r.try_get("sid")?;
            if let Some(sid) = sid.filter(|s| !out.iter().any(|(o, _)| o == s)) {
                out.push((sid, account.to_owned()));
            }
        }
        Ok(out)
    }

    /// The person proved it is them again (`max_age` for the account pages).
    pub async fn session_reauthenticated(&self, sid: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE sso_sessions SET auth_time = now() WHERE id = $1").bind(sid).execute(self.pool()).await?;
        Ok(())
    }

    // Grants ---------------------------------------------------------------------------------

    pub async fn grant_create(&self, g: &NewGrant<'_>) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO grants (id, sso_session_id, sid, client_id, account_id, scope, refresh_hash, act_sub, auth_time,
               amr, idle_expires_at, expires_at, code_hash) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
        )
        .bind(g.id)
        .bind(g.sso_session_id)
        .bind(g.sid)
        .bind(g.client_id)
        .bind(g.account_id)
        .bind(g.scope)
        .bind(g.refresh_hash)
        .bind(g.act_sub)
        .bind(g.auth_time)
        .bind(g.amr)
        .bind(g.idle_expires_at)
        .bind(g.expires_at)
        .bind(g.code_hash)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    /// A grant by id, locked for the rest of `tx` (refresh rotation).
    pub async fn grant_lock(tx: &mut Tx, id: &str) -> Result<Option<Grant>, StoreError> {
        let r = sqlx::query("SELECT * FROM grants WHERE id = $1 FOR UPDATE").bind(id).fetch_optional(&mut **tx).await?;
        r.as_ref().map(grant_of).transpose()
    }

    pub async fn grant(&self, id: &str) -> Result<Option<Grant>, StoreError> {
        let r = sqlx::query("SELECT * FROM grants WHERE id = $1").bind(id).fetch_optional(self.pool()).await?;
        r.as_ref().map(grant_of).transpose()
    }

    /// Rotates a grant's refresh token: the current hash becomes the previous one, the new
    /// token's hash the current one, and the new token, sealed, answers repeats for a while.
    #[allow(clippy::too_many_arguments)]
    pub async fn grant_rotate(
        tx: &mut Tx,
        id: &str,
        new_hash: &[u8],
        successor_enc: &serde_json::Value,
        successor_until: DateTime<Utc>,
        idle_expires_at: Option<DateTime<Utc>>,
        claim_client: Option<&str>,
    ) -> Result<(), StoreError> {
        // The token before the previous one joins the spent ones: presented again, it is reuse.
        sqlx::query(
            "INSERT INTO refresh_spent (hash, grant_id)
             SELECT previous_hash, id FROM grants WHERE id = $1 AND previous_hash IS NOT NULL
             ON CONFLICT DO NOTHING",
        )
        .bind(id)
        .execute(&mut **tx)
        .await?;
        sqlx::query(
            "UPDATE grants SET previous_hash = refresh_hash, refresh_hash = $2, rotated_at = now(), successor_enc = $3,
               successor_until = $4, idle_expires_at = coalesce($5, idle_expires_at), last_used_at = now(),
               client_id = coalesce($6, client_id), legacy_clients = CASE WHEN $6 IS NULL THEN legacy_clients END
             WHERE id = $1",
        )
        .bind(id)
        .bind(new_hash)
        .bind(successor_enc)
        .bind(successor_until)
        .bind(idle_expires_at)
        .bind(claim_client)
        .execute(&mut **tx)
        .await?;
        Ok(())
    }

    /// Whether `hash` is an earlier refresh token of the grant (rotated away before the last).
    pub async fn refresh_spent(tx: &mut Tx, grant_id: &str, hash: &[u8]) -> Result<bool, StoreError> {
        Ok(sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM refresh_spent WHERE hash = $1 AND grant_id = $2)")
            .bind(hash)
            .bind(grant_id)
            .fetch_one(&mut **tx)
            .await?)
    }

    pub async fn grant_revoke(&self, id: &str) -> Result<(), StoreError> {
        sqlx::query("UPDATE grants SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL")
            .bind(id)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// The grant a code produced, revoked (the code was replayed).
    pub async fn grant_revoke_by_code(&self, code_hash: &[u8]) -> Result<Option<String>, StoreError> {
        let r = sqlx::query(
            "UPDATE grants SET revoked_at = now() WHERE code_hash = $1 AND revoked_at IS NULL RETURNING sid",
        )
        .bind(code_hash)
        .fetch_optional(self.pool())
        .await?;
        Ok(r.map(|r| r.try_get("sid")).transpose()?)
    }

    /// Removes what has expired: flows, codes, sessions and grants past their time, plus a day.
    pub async fn oidc_prune(&self) -> Result<(), StoreError> {
        for q in [
            "DELETE FROM auth_flows WHERE expires_at < now() - interval '1 hour'",
            "DELETE FROM auth_codes WHERE expires_at < now() - interval '1 hour'",
            "DELETE FROM grants WHERE expires_at < now() - interval '1 day' OR revoked_at < now() - interval '1 day'",
            "DELETE FROM sso_sessions WHERE expires_at < now() - interval '1 day' OR revoked_at < now() - interval '1 day'",
            "DELETE FROM account_tokens WHERE expires_at < now()",
            // A rotated refresh token's successor is kept, sealed, for its grace window only.
            "UPDATE grants SET successor_enc = NULL WHERE successor_enc IS NOT NULL AND successor_until < now()",
        ] {
            sqlx::query(q).execute(self.pool()).await?;
        }
        Ok(())
    }
}

/// A back-channel logout waiting to be delivered.
#[derive(Debug, Clone)]
pub struct Backchannel {
    pub id: i64,
    pub client_id: String,
    pub claims: serde_json::Value,
    pub created_at: DateTime<Utc>,
}

impl Store {
    /// The clients that hold or held a grant in each of these sessions: (sid, client, account).
    pub async fn clients_of_sessions(&self, sids: &[String]) -> Result<Vec<(String, String, String)>, StoreError> {
        let rows = sqlx::query("SELECT DISTINCT sid, client_id, account_id FROM grants WHERE sid = ANY($1)")
            .bind(sids)
            .fetch_all(self.pool())
            .await?;
        rows.iter().map(|r| Ok((r.try_get("sid")?, r.try_get("client_id")?, r.try_get("account_id")?))).collect()
    }

    /// Whether `client` holds or held a grant in session `sid`.
    pub async fn client_had_session(&self, client: &str, sid: &str) -> Result<bool, StoreError> {
        let r = sqlx::query("SELECT EXISTS (SELECT 1 FROM grants WHERE client_id = $1 AND sid = $2) AS e")
            .bind(client)
            .bind(sid)
            .fetch_one(self.pool())
            .await?;
        Ok(r.try_get("e")?)
    }

    pub async fn backchannel_enqueue(&self, client: &str, claims: &serde_json::Value) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO backchannel_outbox (client_id, claims) VALUES ($1, $2)")
            .bind(client)
            .bind(claims)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Due deliveries, claimed for 30 seconds.
    pub async fn backchannel_due(&self, limit: i64) -> Result<Vec<Backchannel>, StoreError> {
        let rows = sqlx::query(
            "UPDATE backchannel_outbox SET next_attempt_at = now() + interval '30 seconds'
             WHERE id IN (SELECT id FROM backchannel_outbox WHERE done_at IS NULL AND next_attempt_at <= now()
                          ORDER BY id LIMIT $1 FOR UPDATE SKIP LOCKED)
             RETURNING id, client_id, claims, created_at",
        )
        .bind(limit)
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Backchannel {
                    id: r.try_get("id")?,
                    client_id: r.try_get("client_id")?,
                    claims: r.try_get("claims")?,
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect()
    }

    /// Done (`error` empty), or failed: tried again after twice the wait, given up after 10
    /// minutes.
    pub async fn backchannel_result(&self, id: i64, error: &str) -> Result<(), StoreError> {
        if error.is_empty() {
            sqlx::query("UPDATE backchannel_outbox SET done_at = now(), last_error = '' WHERE id = $1")
                .bind(id)
                .execute(self.pool())
                .await?;
        } else {
            sqlx::query(
                "UPDATE backchannel_outbox SET attempts = attempts + 1, last_error = $2,
                   next_attempt_at = now() + interval '5 seconds' * power(2, attempts),
                   done_at = CASE WHEN created_at < now() - interval '10 minutes' THEN now() END
                 WHERE id = $1",
            )
            .bind(id)
            .bind(error.chars().take(300).collect::<String>())
            .execute(self.pool())
            .await?;
        }
        Ok(())
    }

    pub async fn backchannel_prune(&self) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM backchannel_outbox WHERE done_at < now() - interval '1 day'")
            .execute(self.pool())
            .await?;
        Ok(())
    }
}
