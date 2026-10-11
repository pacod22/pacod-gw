//! Google Cloud for the mock: an OAuth token endpoint for service accounts, and Gemini on
//! Vertex AI.
//!
//! - `POST /emulate/google-oauth/token` takes the JWT-bearer grant a service account sends
//!   (<https://developers.google.com/identity/protocols/oauth2/service-account>), checks the
//!   assertion's shape (not its signature: the mock knows no public keys) and issues an access
//!   token. `?expires_in=<seconds>` on the URL shortens the token's life, to test refreshing.
//!   `GET /__mock/google` says how many tokens were issued.
//! - `/emulate/vertex/…` answers as Vertex AI: the native Gemini API below
//!   `/v1/projects/{project}/locations/{location}/publishers/google/models/{model}:{method}`,
//!   with a token the endpoint above issued, or below the project-less express path
//!   `/v1/publishers/google/models/…` with an API key in `x-goog-api-key`. Shapes follow the
//!   Gemini emulation and Vertex's reference; the recorded fixtures under `fixtures/vertex/`
//!   settle them where they exist.

use std::collections::HashSet;

use axum::http::{HeaderMap, Method};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde_json::{Value, json};

use super::gemini::{self, Site};
use super::server::{ApiError, Call, Reply, Shared};

/// The tokens issued so far.
#[derive(Debug, Default)]
pub(crate) struct Tokens {
    issued: HashSet<String>,
    pub(crate) minted: u64,
}

/// `POST /emulate/google-oauth/token`.
pub(crate) fn token(shared: &Shared, call: &Call<'_>) -> Result<Reply, ApiError> {
    let form = std::str::from_utf8(call.body).map_err(|_| ApiError::bad_request("invalid_request: not a form"))?;
    let field = |name: &str| form.split('&').find_map(|pair| pair.strip_prefix(name)?.strip_prefix('='));
    if field("grant_type") != Some("urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer") {
        return Err(ApiError::bad_request("unsupported_grant_type"));
    }
    let assertion = field("assertion").ok_or_else(|| ApiError::bad_request("invalid_request: no assertion"))?;
    let claims = check_assertion(assertion).map_err(|e| ApiError::bad_request(format!("invalid_grant: {e}")))?;
    let expires_in = call
        .query
        .split('&')
        .find_map(|pair| pair.strip_prefix("expires_in="))
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(3600);
    let mut tokens = shared.google.lock();
    tokens.minted += 1;
    let token = format!("mock-access-{}-{:x}", tokens.minted, call.seed);
    tokens.issued.insert(token.clone());
    tracing::debug!(iss = claims["iss"].as_str(), "mock: issued an access token");
    Ok(Reply::Json(json!({ "access_token": token, "expires_in": expires_in, "token_type": "Bearer" })))
}

/// The claims of a well-formed assertion: RS256, issued by an account, for the cloud-platform
/// scope, valid for at most an hour.
fn check_assertion(jwt: &str) -> Result<Value, String> {
    let mut parts = jwt.split('.');
    let mut decode = |what: &str| -> Result<Value, String> {
        let text = parts.next().ok_or(format!("no {what}"))?;
        let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| format!("the {what} is not base64url"))?;
        serde_json::from_slice(&bytes).map_err(|_| format!("the {what} is not JSON"))
    };
    let header = decode("header")?;
    let claims = decode("claims")?;
    if parts.next().is_none_or(str::is_empty) {
        return Err("no signature".into());
    }
    if header["alg"] != "RS256" {
        return Err("alg is not RS256".into());
    }
    let (iat, exp) = (claims["iat"].as_u64().unwrap_or(0), claims["exp"].as_u64().unwrap_or(0));
    let ok = claims["iss"].as_str().is_some_and(|i| i.contains('@'))
        && claims["scope"].as_str().is_some_and(|s| s.contains("cloud-platform"))
        && claims["aud"].as_str().is_some_and(|a| !a.is_empty())
        && exp > iat
        && exp - iat <= 3600;
    if ok { Ok(claims) } else { Err("the claims are incomplete or the lifetime is over an hour".into()) }
}

/// `/emulate/vertex/…`, the prefix already removed.
pub(crate) fn vertex(
    shared: &Shared,
    method: &Method,
    headers: &HeaderMap,
    call: &Call<'_>,
) -> Result<Reply, ApiError> {
    let not_found = || ApiError::new(404, format!("mock: no route for {method} {}", call.path));
    let rest = call.path.strip_prefix("/v1/").or_else(|| call.path.strip_prefix("/v1beta1/")).ok_or_else(not_found)?;
    // Express mode has no project: `publishers/google/…` right after the version.
    let (express, models) = match rest.strip_prefix("publishers/google/") {
        Some(models) => (true, models),
        None => {
            let mut segments = rest.splitn(5, '/');
            let shape = (segments.next(), segments.next(), segments.next(), segments.next());
            let tail = segments.next().unwrap_or("");
            match shape {
                (Some("projects"), Some(_), Some("locations"), Some(_)) => {
                    (false, tail.strip_prefix("publishers/google/").ok_or_else(not_found)?)
                }
                _ => return Err(not_found()),
            }
        }
    };
    authorize(shared, headers, express)?;
    match (method.as_str(), models) {
        ("GET", "models") => Ok(Reply::Json(json!({ "publisherModels": [
            publisher_model("gemini-3.8-flash"), publisher_model("gemini-3.5-flash-lite"), publisher_model("gemini-embedding-001")
        ] }))),
        ("GET", model) if model.starts_with("models/") && !model.contains(':') => {
            let id = &model["models/".len()..];
            if id.starts_with("unknown") || id.starts_with("no-such") {
                return Err(ApiError::new(
                    404,
                    format!("Publisher Model `publishers/google/models/{id}` was not found."),
                ));
            }
            Ok(Reply::Json(publisher_model(id)))
        }
        ("POST", model) if model.starts_with("models/") => gemini::post(call, Site::Vertex),
        _ => Err(not_found()),
    }
}

/// Vertex takes the access token of an account; express mode an API key.
fn authorize(shared: &Shared, headers: &HeaderMap, express: bool) -> Result<(), ApiError> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty());
    if express && header("x-goog-api-key").is_some() {
        return Ok(());
    }
    let token = header("authorization").and_then(|v| v.strip_prefix("Bearer "));
    match token {
        Some(token) if shared.google.lock().issued.contains(token) => Ok(()),
        _ => Err(ApiError::new(
            401,
            "Request had invalid authentication credentials. Expected OAuth 2 access token, login cookie or other valid authentication credential.",
        )),
    }
}

/// A `PublisherModel` as Vertex's reference describes it, abbreviated.
fn publisher_model(id: &str) -> Value {
    json!({
        "name": format!("publishers/google/models/{id}"),
        "versionId": "001",
        "openSourceCategory": "PROPRIETARY",
        "launchStage": "GA",
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assertions_are_checked() {
        let part = |v: Value| URL_SAFE_NO_PAD.encode(v.to_string());
        let header = part(json!({ "alg": "RS256", "typ": "JWT" }));
        let claims = |exp: u64| {
            part(
                json!({ "iss": "a@b.iam.gserviceaccount.com", "scope": "https://www.googleapis.com/auth/cloud-platform",
                         "aud": "https://oauth2.googleapis.com/token", "iat": 1000, "exp": exp }),
            )
        };
        assert!(check_assertion(&format!("{header}.{}.c2ln", claims(4600))).is_ok());
        assert!(check_assertion(&format!("{header}.{}.c2ln", claims(9000))).is_err(), "over an hour");
        assert!(check_assertion(&format!("{header}.{}.", claims(4600))).is_err(), "no signature");
        let none = part(json!({ "alg": "none" }));
        assert!(check_assertion(&format!("{none}.{}.c2ln", claims(4600))).is_err());
    }
}
