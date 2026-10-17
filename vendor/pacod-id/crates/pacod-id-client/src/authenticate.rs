//! Who is calling, in one step: the token is checked (a JWT by its signature, a
//! service token by introspection); a person's ended session and a disabled or deleted
//! principal are refused; the caller's bindings come from the product's copy. A product that
//! assembled these steps itself could drop one; the session check is what makes signing out
//! take effect within seconds rather than when the access token expires.

use std::sync::Arc;

use pacod_id_authz::Binding;

use crate::{AuthzCache, IdClient, Introspector, Verifier, VerifyError};

/// An authenticated caller and the bindings it holds.
#[derive(Debug, Clone)]
pub enum Caller {
    User {
        sub: String,
        sid: String,
        client_id: String,
        scope: String,
        actor: Option<String>,
        /// From the token, when its scope had `email` / `profile`: for display only.
        email: Option<String>,
        name: Option<String>,
        bindings: Vec<Binding>,
    },
    Service {
        sub: String,
        token_id: Option<String>,
        name: String,
        organisation_id: Option<String>,
        bindings: Vec<Binding>,
    },
}

impl Caller {
    pub fn sub(&self) -> &str {
        match self {
            Caller::User { sub, .. } | Caller::Service { sub, .. } => sub,
        }
    }

    pub fn bindings(&self) -> &[Binding] {
        match self {
            Caller::User { bindings, .. } | Caller::Service { bindings, .. } => bindings,
        }
    }
}

/// Why a caller is refused.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("the token is not valid here")]
    Invalid,
    #[error("the token has expired")]
    Expired,
    #[error("the session has ended")]
    SessionEnded,
    #[error("the account is disabled or deleted")]
    Disabled,
    /// pacod-id could not be asked (keys, introspection): the product answers 503, not 401.
    #[error("pacod-id cannot be reached: {0}")]
    Unavailable(String),
}

pub struct Authenticator {
    verifier: Verifier,
    cache: Arc<AuthzCache>,
    introspector: Arc<Introspector>,
}

/// Whether a bearer is a JWT (`{"…` in base64url, three parts) rather than a service token
/// (`pidt_…`, `pgwa_…`).
fn is_jwt(token: &str) -> bool {
    token.starts_with("eyJ") && token.split('.').count() == 3
}

impl Authenticator {
    /// The checks, sharing the product's cache; the introspector forgets tokens the feed revokes.
    pub fn new(client: IdClient, cache: Arc<AuthzCache>) -> Self {
        let introspector = Introspector::new(client.clone());
        introspector.follow(cache.subscribe());
        Authenticator { verifier: Verifier::new(client), cache, introspector }
    }

    pub fn verifier(&self) -> &Verifier {
        &self.verifier
    }

    pub fn cache(&self) -> &Arc<AuthzCache> {
        &self.cache
    }

    pub async fn authenticate(&self, bearer: &str) -> Result<Caller, AuthError> {
        let token = bearer.trim();
        if token.is_empty() {
            return Err(AuthError::Invalid);
        }
        if is_jwt(token) {
            let t = self.verifier.verify(token).await.map_err(|e| match e {
                VerifyError::Expired => AuthError::Expired,
                VerifyError::Invalid(_) => AuthError::Invalid,
                VerifyError::Unavailable(m) => AuthError::Unavailable(m),
            })?;
            if self.cache.is_disabled(&t.sub) {
                return Err(AuthError::Disabled);
            }
            let bindings = self.cache.bindings_of(&t.sub);
            return match (t.kind.as_str(), t.sid) {
                ("user", Some(sid)) => {
                    if self.cache.sid_revoked(&sid) {
                        return Err(AuthError::SessionEnded);
                    }
                    Ok(Caller::User {
                        sub: t.sub,
                        sid,
                        client_id: t.client_id,
                        scope: t.scope,
                        actor: t.actor,
                        email: t.email,
                        name: t.name,
                        bindings,
                    })
                }
                ("service", _) => Ok(Caller::Service {
                    sub: t.sub,
                    token_id: None,
                    name: String::new(),
                    organisation_id: None,
                    bindings,
                }),
                _ => Err(AuthError::Invalid),
            };
        }
        let p = self
            .introspector
            .check(token)
            .await
            .map_err(|e| match e.status() {
                Some(s) if s < 500 => AuthError::Invalid,
                _ => AuthError::Unavailable(e.to_string()),
            })?
            .ok_or(AuthError::Invalid)?;
        if self.cache.is_disabled(&p.sub) {
            return Err(AuthError::Disabled);
        }
        let bindings = self.cache.bindings_of(&p.sub);
        Ok(Caller::Service {
            sub: p.sub,
            token_id: Some(p.token_id),
            name: p.name,
            organisation_id: p.organisation_id,
            bindings,
        })
    }
}
