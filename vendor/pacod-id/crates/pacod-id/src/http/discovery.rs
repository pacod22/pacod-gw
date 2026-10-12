//! OpenID Provider metadata (OIDC Discovery 1.0) per realm, and the JWKS.

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::Shared;
use crate::config::Realm;
use crate::error::ApiError;

/// The realm a browser-facing request is for: the one whose issuer has the request's Host.
pub fn realm_of<'a>(st: &'a Shared, headers: &HeaderMap) -> Result<&'a Realm, ApiError> {
    let host = headers.get(header::HOST).and_then(|h| h.to_str().ok()).unwrap_or_default();
    st.config.realm_for_host(host).ok_or_else(|| ApiError::not_found(format!("no realm is served at {host:?}")))
}

/// The provider metadata of a realm.
pub fn metadata(realm: &Realm) -> Value {
    let i = &realm.issuer;
    json!({
        "issuer": i,
        "authorization_endpoint": format!("{i}/oidc/authorize"),
        "token_endpoint": format!("{i}/oidc/token"),
        "userinfo_endpoint": format!("{i}/oidc/userinfo"),
        "jwks_uri": format!("{i}/oidc/jwks"),
        "end_session_endpoint": format!("{i}/oidc/logout"),
        "revocation_endpoint": format!("{i}/oidc/revoke"),
        "scopes_supported": ["openid", "email", "profile", "offline_access"],
        "response_types_supported": ["code"],
        "response_modes_supported": ["query"],
        "grant_types_supported": [
            "authorization_code",
            "refresh_token",
            "client_credentials",
            "urn:ietf:params:oauth:grant-type:token-exchange"
        ],
        "subject_types_supported": ["public"],
        // OIDC Discovery requires RS256 here; ES256 is available to a client that asks.
        "id_token_signing_alg_values_supported": ["RS256", "ES256"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
        "revocation_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post", "none"],
        "code_challenge_methods_supported": ["S256"],
        "claims_supported": [
            "iss", "sub", "aud", "azp", "exp", "iat", "auth_time", "nonce", "sid", "amr", "at_hash",
            "email", "email_verified", "name", "locale"
        ],
        // consent and select_account are accepted and have nothing to do (first-party clients,
        // one account per browser); they are not offered as features.
        "prompt_values_supported": ["none", "login", "create"],
        "ui_locales_supported": realm.locales,
        "claims_parameter_supported": false,
        "request_parameter_supported": false,
        "request_uri_parameter_supported": false,
        "require_request_uri_registration": false,
        "authorization_response_iss_parameter_supported": true,
        "backchannel_logout_supported": true,
        "backchannel_logout_session_supported": true,
        "frontchannel_logout_supported": false
    })
}

pub async fn configuration(State(st): State<Shared>, headers: HeaderMap) -> Result<Response, ApiError> {
    let realm = realm_of(&st, &headers)?;
    let mut response = Json(metadata(realm)).into_response();
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=300"));
    response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    Ok(response)
}

pub async fn jwks(State(st): State<Shared>) -> Response {
    let mut response = Json(st.keys.jwks()).into_response();
    // Clients refetch on an unknown kid; an hour of caching is safe since a new key is
    // published a day before it signs.
    response.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=3600"));
    response.headers_mut().insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, HeaderValue::from_static("*"));
    response
}
