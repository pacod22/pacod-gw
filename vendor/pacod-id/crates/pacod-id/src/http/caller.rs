//! Who calls the internal API: a product's service (a client-credentials token, or HTTP Basic
//! with its secret), or a confidential client (HTTP Basic), for example a BFF ending a session.

use axum::extract::FromRequestParts;
use axum::http::header;
use axum::http::request::Parts;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::Utc;
use pacod_id_jose::Alg;

use super::Shared;
use crate::config::{Client, ClientKind};
use crate::crypto::ct_eq;
use crate::error::ApiError;

/// An authenticated client of the internal API.
#[derive(Debug, Clone)]
pub struct CallerClient {
    pub id: String,
    pub kind: ClientKind,
    pub realm: String,
    /// For a service: its product.
    pub product: Option<String>,
}

impl CallerClient {
    fn of(c: &Client) -> Self {
        CallerClient { id: c.id.clone(), kind: c.kind, realm: c.realm.clone(), product: c.product.clone() }
    }

    pub fn is_service(&self) -> bool {
        self.kind == ClientKind::Service
    }
}

fn denied() -> ApiError {
    ApiError::unauthorized("send a service token or the client's credentials")
}

impl FromRequestParts<Shared> for CallerClient {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, st: &Shared) -> Result<Self, ApiError> {
        let auth = parts.headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).ok_or_else(denied)?;
        if let Some(token) = auth.strip_prefix("Bearer ").or_else(|| auth.strip_prefix("bearer ")) {
            let v = st.keys.verifier().verify(token.trim(), &[Alg::RS256, Alg::ES256]).map_err(|_| denied())?;
            let c = &v.claims;
            let ok = v.header.typ.as_deref() == Some("at+jwt")
                && c["aud"] == "pacod-id"
                && c["pid_kind"] == "service"
                && c["exp"].as_i64().is_some_and(|e| e > Utc::now().timestamp());
            let client = c["sub"]
                .as_str()
                .and_then(|s| st.config.clients.get(s))
                .filter(|cl| ok && cl.kind == ClientKind::Service && c["iss"] == st.config.realms[&cl.realm].issuer)
                .ok_or_else(denied)?;
            return Ok(CallerClient::of(client));
        }
        let basic = auth.strip_prefix("Basic ").or_else(|| auth.strip_prefix("basic ")).ok_or_else(denied)?;
        let pair = STANDARD.decode(basic.trim()).ok().and_then(|r| String::from_utf8(r).ok()).ok_or_else(denied)?;
        let (id, secret) = pair.split_once(':').ok_or_else(denied)?;
        let (id, secret) = (crate::oidc::form_decode(id), crate::oidc::form_decode(secret));
        let client = st.config.clients.get(&id).filter(|c| c.kind != ClientKind::Public).ok_or_else(denied)?;
        let hash = st.boot.token_pepper.hash(&secret);
        let stored = st.store.client_secrets().await?;
        if stored.iter().any(|(c, h)| *c == id && ct_eq(h, &hash)) {
            Ok(CallerClient::of(client))
        } else {
            Err(denied())
        }
    }
}
