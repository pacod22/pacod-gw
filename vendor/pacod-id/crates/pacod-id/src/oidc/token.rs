//! The token endpoint: authorization codes with PKCE, refresh tokens
//! rotated on every use, client credentials for product services, and token exchange.

use std::collections::BTreeMap;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, header};
use axum::response::Response;
use chrono::{Duration, Utc};
use serde_json::json;

use super::{
    OAuthError, Subject, access_token, authenticate_client, id_token, new_grant, pkce_matches, token_response,
};
use crate::config::{Client, ClientKind, Grant, Realm};
use crate::crypto::{envelope, sha256, tokens};
use crate::http::Shared;
use crate::mail::{self, Template};
use crate::store::{Account, AuditEntry, Store, changes};

/// The form of a token, revocation or introspection request. Only form encoding, and no
/// parameter twice (RFC 6749, 3.2).
pub fn form(headers: &HeaderMap, body: &Bytes) -> Result<BTreeMap<String, String>, OAuthError> {
    let ct = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default();
    if !ct.to_ascii_lowercase().starts_with("application/x-www-form-urlencoded") {
        return Err(OAuthError::invalid_request(
            "send the parameters form-encoded (application/x-www-form-urlencoded)",
        ));
    }
    let mut out = BTreeMap::new();
    for (k, v) in url::form_urlencoded::parse(body) {
        if out.insert(k.clone().into_owned(), v.into_owned()).is_some() {
            return Err(OAuthError::invalid_request(format!("{k} appears twice")));
        }
    }
    Ok(out)
}

fn realm_of<'a>(st: &'a Shared, client: &Client) -> &'a Realm {
    &st.config.realms[&client.realm]
}

fn needs(form: &BTreeMap<String, String>, name: &str) -> Result<String, OAuthError> {
    form.get(name)
        .filter(|v| !v.is_empty())
        .cloned()
        .ok_or_else(|| OAuthError::invalid_request(format!("{name} is missing")))
}

fn lifetime(client: &Client) -> Duration {
    Duration::seconds(i64::from(client.access_ttl_seconds))
}

pub async fn token(State(st): State<Shared>, headers: HeaderMap, body: Bytes) -> Result<Response, OAuthError> {
    let form = form(&headers, &body)?;
    let client = authenticate_client(&st, &headers, &form).await?;
    let grant_type = needs(&form, "grant_type")?;
    let allowed = |g: Grant| client.grants().contains(&g);
    match grant_type.as_str() {
        "authorization_code" if allowed(Grant::AuthorizationCode) => code_grant(&st, client, &form).await,
        // A service refreshes the delegated grants it got by token exchange.
        "refresh_token" if allowed(Grant::RefreshToken) || allowed(Grant::TokenExchange) => {
            refresh_grant(&st, client, &form).await
        }
        "client_credentials" if allowed(Grant::ClientCredentials) => client_credentials(&st, client).await,
        "urn:ietf:params:oauth:grant-type:token-exchange" if allowed(Grant::TokenExchange) => {
            super::exchange::exchange(&st, client, &form).await
        }
        "authorization_code"
        | "refresh_token"
        | "client_credentials"
        | "urn:ietf:params:oauth:grant-type:token-exchange" => {
            Err(OAuthError::new("unauthorized_client", "this client may not use this grant type"))
        }
        _ => Err(OAuthError::new("unsupported_grant_type", "unknown grant type")),
    }
}

/// An active account, or an invalid grant.
async fn active_account(store: &Store, id: &str) -> Result<Account, OAuthError> {
    store
        .account(id)
        .await?
        .filter(Account::is_active)
        .ok_or_else(|| OAuthError::invalid_grant("the account is not active"))
}

async fn code_grant(st: &Shared, client: &Client, form: &BTreeMap<String, String>) -> Result<Response, OAuthError> {
    let code = needs(form, "code")?;
    let redirect_uri = needs(form, "redirect_uri")?;
    let verifier = needs(form, "code_verifier")?;
    let code_hash = sha256(code.as_bytes());
    // Another client's code is unknown here: it neither redeems, spends nor revokes anything.
    let Some(c) = st.store.code_take(&code_hash, &client.id).await? else {
        return Err(OAuthError::invalid_grant("unknown or expired code"));
    };
    if c.used_before {
        // A replayed code: what it produced goes too (RFC 6749, 4.1.2).
        if let Some(sid) = st.store.grant_revoke_by_code(&code_hash).await? {
            st.store
                .audit(
                    &AuditEntry::new("code_replayed")
                        .subject(&c.account_id)
                        .detail(json!({ "sid": sid, "client": client.id })),
                )
                .await?;
        }
        return Err(OAuthError::invalid_grant("the code was used already"));
    }
    if c.expires_at < Utc::now() {
        return Err(OAuthError::invalid_grant("unknown or expired code"));
    }
    if c.redirect_uri != redirect_uri {
        return Err(OAuthError::invalid_grant("redirect_uri does not match the authorization request"));
    }
    if !pkce_matches(&verifier, &c.pkce_challenge) {
        return Err(OAuthError::invalid_grant("the code_verifier does not match the code_challenge"));
    }
    let account = active_account(&st.store, &c.account_id).await?;
    let session = match &c.sso_session_id {
        Some(sid) => Some(
            st.store
                .session(sid)
                .await?
                .filter(|s| s.is_live())
                .ok_or_else(|| OAuthError::invalid_grant("the session has ended"))?,
        ),
        None => None,
    };
    let sid = c.sso_session_id.clone().unwrap_or_default();
    let amr = session.as_ref().map(|s| s.amr.clone()).unwrap_or_default();
    let sub = Subject { account: &account, sid: &sid, auth_time: c.auth_time, amr: &amr, scope: &c.scope };
    let realm = realm_of(st, client);
    let access = access_token(st, realm, client, &sub, None, None, lifetime(client))?;
    let roles = super::roles_of(st, client, &account.id).await?;
    let id = id_token(st, realm, client, &sub, c.nonce.as_deref(), &access, roles.as_deref())?;
    let mut body = json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": client.access_ttl_seconds,
        "id_token": id,
        "scope": c.scope,
    });
    if client.grants().contains(&Grant::RefreshToken) {
        let bound =
            session.as_ref().filter(|_| client.kind == ClientKind::Confidential).map(|s| (s.id.as_str(), s.expires_at));
        body["refresh_token"] = json!(new_grant(st, client, &sub, bound, None, Some(&code_hash)).await?);
    }
    Ok(token_response(body))
}

async fn refresh_grant(st: &Shared, client: &Client, form: &BTreeMap<String, String>) -> Result<Response, OAuthError> {
    let presented = needs(form, "refresh_token")?;
    let invalid = || OAuthError::invalid_grant("unknown, expired or revoked refresh token");
    let id = tokens::id_of(&presented, tokens::REFRESH_PREFIX).map(|bare| format!("grt_{bare}")).ok_or_else(invalid)?;
    let hash = sha256(presented.as_bytes());
    let mut tx = st.store.begin().await?;
    let grant = Store::grant_lock(&mut tx, &id).await?.ok_or_else(invalid)?;
    // Whose grant: the client's own, or an imported one any of a list of clients may claim.
    let claim = match &grant.legacy_clients {
        Some(list) if list.contains(&client.id) => Some(client.id.as_str()),
        _ if grant.client_id == client.id => None,
        _ => return Err(invalid()),
    };
    let now = Utc::now();
    if grant.revoked_at.is_some() || grant.expires_at <= now || grant.idle_expires_at.is_some_and(|t| t <= now) {
        return Err(invalid());
    }

    let token = if crate::crypto::ct_eq(&hash, &grant.refresh_hash) {
        // The current token: rotate. The successor answers a repeat for 30 seconds.
        let next = tokens::token(tokens::REFRESH_PREFIX, &grant.id);
        let sealed = envelope::seal(st.boot.kek(), &grant.id, next.as_bytes());
        let idle =
            grant.idle_expires_at.map(|_| now + Duration::days(i64::from(client.refresh_idle_days.unwrap_or(7))));
        Store::grant_rotate(
            &mut tx,
            &grant.id,
            &sha256(next.as_bytes()),
            &serde_json::to_value(sealed).map_err(OAuthError::server)?,
            now + super::REFRESH_GRACE,
            idle,
            claim,
        )
        .await?;
        next
    } else if grant.previous_hash.as_deref().is_some_and(|p| crate::crypto::ct_eq(&hash, p))
        && grant.successor_until.is_some_and(|t| t > now)
    {
        // A repeat of the token just rotated (two tabs at once): the same successor.
        // Sealed under a key-encryption key this instance no longer has: the repeat is refused
        // like any late one, rather than failing.
        let sealed: envelope::Sealed =
            serde_json::from_value(grant.successor_enc.clone().ok_or_else(invalid)?).map_err(|_| invalid())?;
        let raw = envelope::open(&st.boot.keks, &grant.id, &sealed).map_err(|_| invalid())?;
        String::from_utf8(raw.to_vec()).map_err(OAuthError::server)?
    } else if !(grant.previous_hash.as_deref().is_some_and(|p| crate::crypto::ct_eq(&hash, p))
        || Store::refresh_spent(&mut tx, &grant.id, &hash).await?)
    {
        // A secret this grant never had: someone who knows only the grant's id (it is not
        // secret, a redacted log shows it) must not be able to sign the owner out.
        return Err(invalid());
    } else {
        // An older token used again: it may have been copied. Every session and grant of the
        // account ends, and the owner is told.
        let revoked = Store::sessions_revoke_all(&mut tx, &grant.account_id, "refresh_token_reused").await?;
        changes::sessions_revoked(&mut tx, &revoked, "refresh_token_reused").await?;
        tx.commit().await?;
        st.store.account_tokens_delete(&grant.account_id, "change_email").await?;
        super::logout::notify_clients(st, &revoked).await?;
        reuse_detected(st, client, &grant.account_id).await?;
        return Err(invalid());
    };
    tx.commit().await?;

    let account = active_account(&st.store, &grant.account_id).await?;
    if let Some(sid) = &grant.sso_session_id
        && !st.store.session(sid).await?.is_some_and(|s| s.is_live())
    {
        return Err(invalid());
    }
    let sub = Subject {
        account: &account,
        sid: &grant.sid,
        auth_time: grant.auth_time,
        amr: &grant.amr,
        scope: &grant.scope,
    };
    let realm = realm_of(st, client);
    let audience = if grant.act_sub.is_some() { client.audience.as_deref() } else { None };
    let access = access_token(st, realm, client, &sub, grant.act_sub.as_deref(), audience, lifetime(client))?;
    let mut body = json!({
        "access_token": access,
        "token_type": "Bearer",
        "expires_in": client.access_ttl_seconds,
        "refresh_token": token,
        "scope": grant.scope,
    });
    if grant.scope.split(' ').any(|s| s == "openid") && grant.act_sub.is_none() {
        let roles = super::roles_of(st, client, &sub.account.id).await?;
        body["id_token"] = json!(id_token(st, realm, client, &sub, None, &access, roles.as_deref())?);
    }
    Ok(token_response(body))
}

/// Tells the owner of an account that every session was ended because a refresh token came
/// back, and records it.
async fn reuse_detected(st: &Shared, client: &Client, account: &str) -> Result<(), OAuthError> {
    st.store
        .audit(&AuditEntry::new("refresh_token_reused").subject(account).detail(json!({ "client": client.id })))
        .await?;
    if let Some(a) = st.store.account(account).await? {
        let realm = realm_of(st, client);
        let params = BTreeMap::from([("link".to_owned(), format!("{}/login", realm.issuer))]);
        mail::enqueue(st, Template::SessionRevokedReuse, &realm.name, &a.email, params).await?;
    }
    Ok(())
}

/// A product's service: a short token for pacod-id's internal API.
async fn client_credentials(st: &Shared, client: &Client) -> Result<Response, OAuthError> {
    let now = Utc::now();
    let realm = realm_of(st, client);
    let claims = json!({
        "iss": realm.issuer,
        "sub": client.id,
        "aud": "pacod-id",
        "client_id": client.id,
        "iat": now.timestamp(),
        "exp": (now + lifetime(client)).timestamp(),
        "jti": tokens::opaque(),
        "pid_kind": "service",
    });
    let token = super::sign(st, client.access_token_alg, "at+jwt", &claims)?;
    Ok(token_response(
        json!({ "access_token": token, "token_type": "Bearer", "expires_in": client.access_ttl_seconds }),
    ))
}
