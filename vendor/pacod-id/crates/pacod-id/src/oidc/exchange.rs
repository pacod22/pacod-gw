//! Token exchange (RFC 8693), for first-party product services only.
//!
//! - A product's facade swaps a caller's token for a 60-second token for pacod-id's admin API
//!   (`aud` pacod-id, `act.sub` the product's client): pacod-id then decides on the caller's
//!   own bindings.
//! - With `requested_token_type` refresh_token, a delegated grant: the gateway's tester keeps
//!   working for an admin through a long run. It is bound to the admin's session, so signing
//!   out stops it.
//!
//! The subject token is a user access token for the service's own product (checked: signature,
//! `typ` at+jwt, issuer, expiry, audience, live session, active account), or a service token
//! (pidt_, or an imported pgwa_).

use std::collections::BTreeMap;

use axum::response::Response;
use chrono::{Duration, Utc};
use pacod_id_jose::Alg;
use serde_json::{Value, json};

use super::{OAuthError, Subject, access_token, new_grant, sign, token_response};
use crate::config::Client;
use crate::crypto::tokens;
use crate::http::Shared;
use crate::store::Account;

pub const ACCESS_TOKEN: &str = "urn:ietf:params:oauth:token-type:access_token";
pub const REFRESH_TOKEN: &str = "urn:ietf:params:oauth:token-type:refresh_token";
/// The lifetime of a token for pacod-id's admin API.
const FACADE_LIFETIME: Duration = Duration::seconds(60);
/// A delegated grant ends with its session, and after 12 hours at the latest.
const DELEGATED_FOR: Duration = Duration::hours(12);

/// Who a subject token stands for.
pub enum Holder {
    User { account: Box<Account>, sid: String, auth_time: chrono::DateTime<Utc>, amr: Vec<String>, scope: String },
    Service { id: String, name: String },
}

/// The prefix of a product's admin tokens imported as service tokens, by product.
pub fn legacy_prefix(product: &str) -> Option<&'static str> {
    match product {
        "gw" => Some("pgwa"),
        _ => None,
    }
}

/// Resolves a service token: pidt_ under PID_TOKEN_PEPPER, or an imported one under its
/// product's pepper.
pub async fn service_token(
    st: &Shared,
    token: &str,
) -> Result<Option<crate::store::service::ServiceToken>, OAuthError> {
    if tokens::id_of(token, tokens::SERVICE_TOKEN_PREFIX).is_some() {
        return Ok(st.store.service_token(&st.boot.token_pepper.hash(token), None).await?);
    }
    for (product, pepper) in &st.boot.legacy_peppers {
        if let Some(prefix) = legacy_prefix(product)
            && tokens::id_of(token, prefix).is_some()
        {
            return Ok(st.store.service_token(&pepper.hash(token), Some(product)).await?);
        }
    }
    Ok(None)
}

/// Checks a user access token pacod-id issued, for the audience `audience`.
pub async fn user_access_token(st: &Shared, token: &str, audience: Option<&str>) -> Result<Option<Holder>, OAuthError> {
    let Ok(verified) = st.keys.verifier().verify(token, &[Alg::RS256, Alg::ES256]) else { return Ok(None) };
    // An id_token is signed by the same keys: only at+jwt counts here.
    if verified.header.typ.as_deref() != Some("at+jwt") {
        return Ok(None);
    }
    let c = &verified.claims;
    let now = Utc::now().timestamp();
    let iss_ok = c["iss"].as_str().is_some_and(|i| st.config.realms.values().any(|r| r.issuer == i));
    let exp_ok = c["exp"].as_i64().is_some_and(|e| e > now);
    let aud_ok = audience
        .is_none_or(|a| c["aud"].as_str() == Some(a) || c["aud"].as_array().is_some_and(|l| l.iter().any(|x| x == a)));
    if !iss_ok || !exp_ok || !aud_ok || c["pid_kind"] != "user" {
        return Ok(None);
    }
    let (Some(sub), Some(sid)) = (c["sub"].as_str(), c["sid"].as_str()) else { return Ok(None) };
    if !st.store.session(sid).await?.is_some_and(|s| s.is_live() && s.account_id == sub) {
        return Ok(None);
    }
    let Some(account) = st.store.account(sub).await?.filter(Account::is_active) else { return Ok(None) };
    let amr = c["amr"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_owned)).collect())
        .unwrap_or_default();
    Ok(Some(Holder::User {
        account: Box::new(account),
        sid: sid.to_owned(),
        auth_time: c["auth_time"]
            .as_i64()
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0))
            .unwrap_or_else(Utc::now),
        amr,
        scope: c["scope"].as_str().unwrap_or_default().to_owned(),
    }))
}

pub async fn exchange(st: &Shared, client: &Client, form: &BTreeMap<String, String>) -> Result<Response, OAuthError> {
    let subject_token =
        form.get("subject_token").ok_or_else(|| OAuthError::invalid_request("subject_token is missing"))?;
    if form.get("subject_token_type").map(String::as_str) != Some(ACCESS_TOKEN) {
        return Err(OAuthError::invalid_request("subject_token_type must be the access token type"));
    }
    let requested = form.get("requested_token_type").map(String::as_str).unwrap_or(ACCESS_TOKEN);
    let audience = form.get("audience").map(String::as_str).unwrap_or("pacod-id");
    if form.contains_key("actor_token") {
        return Err(OAuthError::invalid_request("actor tokens are not supported: the client acts"));
    }
    let realm = &st.config.realms[&client.realm];

    // Only tokens the product's callers sent it: their audience is the product's (the
    // configuration requires one of every client that exchanges).
    let own_audience = client
        .audience
        .as_deref()
        .ok_or_else(|| OAuthError::new("unauthorized_client", "this client has no audience to exchange tokens of"))?;
    let holder = match service_token(st, subject_token).await? {
        Some(t) => Holder::Service { id: t.service_account_id, name: t.service_account_name },
        None => user_access_token(st, subject_token, Some(own_audience))
            .await?
            .ok_or_else(|| OAuthError::invalid_grant("the subject token is not valid here"))?,
    };

    match (requested, audience, holder) {
        (ACCESS_TOKEN, "pacod-id", Holder::User { account, sid, auth_time, amr, scope }) => {
            let sub = Subject { account: &account, sid: &sid, auth_time, amr: &amr, scope: &scope };
            let token = access_token(st, realm, client, &sub, Some(&client.id), Some("pacod-id"), FACADE_LIFETIME)?;
            Ok(issued(token, ACCESS_TOKEN, None))
        }
        (ACCESS_TOKEN, "pacod-id", Holder::Service { id, name }) => {
            let now = Utc::now();
            let claims: Value = json!({
                "iss": realm.issuer,
                "sub": id,
                "aud": "pacod-id",
                "client_id": client.id,
                "iat": now.timestamp(),
                "exp": (now + FACADE_LIFETIME).timestamp(),
                "jti": tokens::opaque(),
                "act": { "sub": client.id },
                "pid_kind": "service",
                "name": name,
            });
            Ok(issued(sign(st, client.access_token_alg, "at+jwt", &claims)?, ACCESS_TOKEN, None))
        }
        (REFRESH_TOKEN, _, Holder::User { account, sid, auth_time, amr, scope }) => {
            let session = st
                .store
                .session(&sid)
                .await?
                .filter(|s| s.is_live())
                .ok_or_else(|| OAuthError::invalid_grant("the session has ended"))?;
            let sub = Subject { account: &account, sid: &sid, auth_time, amr: &amr, scope: &scope };
            let until = session.expires_at.min(Utc::now() + DELEGATED_FOR);
            let refresh = new_grant(st, client, &sub, Some((&sid, until)), Some(&client.id), None).await?;
            let token = access_token(
                st,
                realm,
                client,
                &sub,
                Some(&client.id),
                client.audience.as_deref(),
                Duration::seconds(i64::from(client.access_ttl_seconds)),
            )?;
            // The access_token field holds an access token; the delegated grant is the refresh token.
            Ok(issued(token, ACCESS_TOKEN, Some(refresh)))
        }
        (REFRESH_TOKEN, _, Holder::Service { .. }) => {
            Err(OAuthError::invalid_request("a service token needs no delegated grant: it lasts by itself"))
        }
        _ => Err(OAuthError::new("invalid_target", "tokens are exchanged for pacod-id's API only")),
    }
}

fn issued(access: String, issued_type: &str, refresh: Option<String>) -> Response {
    let mut body = json!({
        "access_token": access,
        "issued_token_type": issued_type,
        "token_type": "Bearer",
    });
    if let Some(r) = refresh {
        body["refresh_token"] = json!(r);
    }
    token_response(body)
}
