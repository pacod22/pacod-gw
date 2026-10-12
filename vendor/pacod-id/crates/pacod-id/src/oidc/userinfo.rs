//! Userinfo (OIDC Core 5.3), token revocation (RFC 7009) and introspection (RFC 7662, on the
//! internal listener, for products' services).

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router, routing::get, routing::post};
use chrono::Utc;
use pacod_id_jose::Alg;
use serde_json::{Value, json};

use super::exchange::{Holder, service_token, user_access_token};
use super::token::form;
use super::{OAuthError, authenticate_client, no_store};
use crate::crypto::tokens;
use crate::http::Shared;
use crate::http::caller::CallerClient;

pub fn public_routes() -> Router<Shared> {
    Router::new().route("/oidc/userinfo", get(userinfo).post(userinfo)).route("/oidc/revoke", post(revoke))
}

pub fn internal_routes() -> Router<Shared> {
    Router::new()
        .route("/oidc/userinfo", get(userinfo).post(userinfo))
        .route("/oidc/revoke", post(revoke))
        .route("/internal/v1/introspect", post(introspect))
}

fn invalid_token() -> Response {
    let mut r = (StatusCode::UNAUTHORIZED, Json(json!({ "error": "invalid_token" }))).into_response();
    r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static(r#"Bearer error="invalid_token""#));
    no_store(r.headers_mut());
    r
}

async fn userinfo(State(st): State<Shared>, headers: HeaderMap) -> Response {
    let Some(token) = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
    else {
        let mut r = (StatusCode::UNAUTHORIZED, Json(json!({ "error": "invalid_request" }))).into_response();
        r.headers_mut().insert(header::WWW_AUTHENTICATE, HeaderValue::from_static(r#"Bearer realm="pacod-id""#));
        return r;
    };
    // A failure on our side is a server error: an invalid_token would sign the person out.
    let server_error = |e: &dyn std::fmt::Display| {
        tracing::error!(error = %e, "userinfo failed");
        let mut r = (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({ "error": "server_error" }))).into_response();
        no_store(r.headers_mut());
        r
    };
    let (account, scope) = match user_access_token(&st, token.trim(), None).await {
        Ok(Some(Holder::User { account, scope, .. })) => (account, scope),
        Ok(_) => return invalid_token(),
        Err(e) => return server_error(&e.description),
    };
    let has = |s: &str| scope.split(' ').any(|x| x == s);
    let mut body = json!({ "sub": account.id });
    if has("email") {
        body["email"] = json!(account.email);
        body["email_verified"] = json!(account.is_verified());
    }
    if has("profile") {
        // An empty claim is left out (OIDC Core 5.3.2), not sent as "".
        if !account.display_name.is_empty() {
            body["name"] = json!(account.display_name);
        }
        if !account.locale.is_empty() {
            body["locale"] = json!(account.locale);
        }
    }
    let client = st
        .keys
        .verifier()
        .verify(token.trim(), &[Alg::RS256, Alg::ES256])
        .ok()
        .and_then(|v| v.claims["client_id"].as_str().and_then(|c| st.config.clients.get(c)));
    if let Some(client) = client {
        match super::roles_of(&st, client, &account.id).await {
            Ok(Some(roles)) => body["roles"] = json!(roles),
            Ok(None) => {}
            Err(e) => return server_error(&e),
        }
    }
    let mut r = Json(body).into_response();
    no_store(r.headers_mut());
    r
}

/// Revokes a refresh token of the calling client. Always 200: whether the token was known stays
/// unsaid (RFC 7009, 2.2).
async fn revoke(State(st): State<Shared>, headers: HeaderMap, body: Bytes) -> Result<Response, OAuthError> {
    let form = form(&headers, &body)?;
    let client = authenticate_client(&st, &headers, &form).await?;
    let token = form.get("token").ok_or_else(|| OAuthError::invalid_request("token is missing"))?;
    if let Some(bare) = tokens::id_of(token, tokens::REFRESH_PREFIX)
        && let Some(grant) = st.store.grant(&format!("grt_{bare}")).await?
        && (grant.client_id == client.id || grant.legacy_clients.as_ref().is_some_and(|l| l.contains(&client.id)))
        && crate::crypto::ct_eq(&crate::crypto::sha256(token.as_bytes()), &grant.refresh_hash)
    {
        st.store.grant_revoke(&grant.id).await?;
    }
    let mut r = StatusCode::OK.into_response();
    no_store(r.headers_mut());
    Ok(r)
}

/// What a token is (RFC 7662), for a product's service: a service token (pidt_, or an imported
/// pgwa_) or an access token pacod-id issued. Anything else is inactive, without a reason.
async fn introspect(
    State(st): State<Shared>,
    caller: CallerClient,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, OAuthError> {
    if !caller.is_service() {
        return Err(OAuthError::new("unauthorized_client", "introspection is for products' services"));
    }
    let form = form(&headers, &body)?;
    let token = form.get("token").ok_or_else(|| OAuthError::invalid_request("token is missing"))?;
    let inactive = || json!({ "active": false });
    let answer: Value = if let Some(t) = service_token(&st, token).await? {
        json!({
            "active": true,
            "sub": t.service_account_id,
            "pid_kind": "service",
            "token_id": t.token_id,
            "name": t.service_account_name,
            "organisation_id": t.organisation_id,
            "exp": t.expires_at.map(|e| e.timestamp()),
        })
    } else {
        match st.keys.verifier().verify(token, &[Alg::RS256, Alg::ES256]) {
            Ok(v)
                if v.header.typ.as_deref() == Some("at+jwt")
                    && v.claims["exp"].as_i64().is_some_and(|e| e > Utc::now().timestamp()) =>
            {
                let c = v.claims;
                let live = match c["pid_kind"].as_str() {
                    Some("user") => matches!(user_access_token(&st, token, None).await?, Some(Holder::User { .. })),
                    // A product's own client-credentials token (sub: the client) lives while the
                    // client is configured; a service account's exchanged token while the account
                    // is active: a disabled or deleted one's tokens are dead at once, as for people.
                    Some("service") if c["sub"].as_str().is_some_and(|s| st.config.clients.contains_key(s)) => true,
                    Some("service") => {
                        let active: Option<bool> =
                            sqlx::query_scalar("SELECT status = 'active' FROM service_accounts WHERE id = $1")
                                .bind(c["sub"].as_str().unwrap_or_default())
                                .fetch_optional(st.store.pool())
                                .await
                                .map_err(OAuthError::server)?;
                        active == Some(true)
                    }
                    _ => false,
                };
                if live {
                    json!({
                        "active": true,
                        "sub": c["sub"], "pid_kind": c["pid_kind"], "client_id": c["client_id"], "aud": c["aud"],
                        "scope": c["scope"], "sid": c["sid"], "iss": c["iss"], "iat": c["iat"], "exp": c["exp"],
                        "token_type": "Bearer",
                    })
                } else {
                    inactive()
                }
            }
            _ => inactive(),
        }
    };
    let mut r = Json(answer).into_response();
    no_store(r.headers_mut());
    Ok(r)
}
