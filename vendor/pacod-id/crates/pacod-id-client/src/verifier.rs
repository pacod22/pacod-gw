//! Checks the access tokens a product receives: pacod-id's signature
//! (keys from the internal listener), `typ` at+jwt, one of the product's issuers, the product's
//! audience, and the time, with 30 seconds of leeway. id_tokens, logout tokens and tokens for
//! pacod-id's own API are refused.

use std::sync::Arc;
use std::time::{Duration, Instant};

use arc_swap::ArcSwap;
use pacod_id_jose::{Alg, JwkSet};
use serde_json::Value;

use crate::IdClient;

const LEEWAY: i64 = 30;
/// Keys are read for an unknown key id at most this often, whether the last read worked or not.
const REFETCH_AT_MOST: Duration = Duration::from_secs(30);
/// Known keys are read again this often (by default), in the background: no request waits.
const REFRESH_EVERY: Duration = Duration::from_secs(3600);
/// A read of the keys that takes longer is given up.
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// Why a token is refused. Callers answer 401 for all of them except `Unavailable`.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("not a token of pacod-id for this product: {0}")]
    Invalid(String),
    #[error("the token has expired")]
    Expired,
    #[error("pacod-id's keys cannot be read: {0}")]
    Unavailable(String),
}

/// What a checked access token says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessToken {
    pub sub: String,
    /// `user` or `service` (a service account's token exchanged by a product).
    pub kind: String,
    /// The SSO session, for a person's token.
    pub sid: Option<String>,
    pub client_id: String,
    pub scope: String,
    pub issuer: String,
    /// The service acting for the subject (`act.sub`), when there is one.
    pub actor: Option<String>,
    pub expires_at: i64,
    /// The person's address and name, when the token's scope included `email` and `profile`:
    /// for showing who acts, never for deciding what they may do.
    pub email: Option<String>,
    pub name: Option<String>,
}

struct Keys {
    verifier: pacod_id_jose::Verifier,
    read_at: Option<Instant>,
}

struct Inner {
    client: IdClient,
    keys: ArcSwap<Keys>,
    /// Held while the keys are read: callers that need them wait for that read, not their own.
    reading: tokio::sync::Mutex<()>,
    /// When a read was last started, successful or not.
    tried_at: std::sync::Mutex<Option<Instant>>,
    refresh_every: Duration,
}

/// Checks access tokens against pacod-id's keys. Cheap to clone; clones share the keys.
#[derive(Clone)]
pub struct Verifier {
    inner: Arc<Inner>,
}

impl Inner {
    /// Takes the right to start a read, at most once per `REFETCH_AT_MOST`.
    fn claim_read(&self) -> bool {
        let mut tried = self.tried_at.lock().expect("not poisoned");
        if tried.is_some_and(|t| t.elapsed() < REFETCH_AT_MOST) {
            return false;
        }
        *tried = Some(Instant::now());
        true
    }

    async fn read(&self) -> Result<(), VerifyError> {
        let _one = self.reading.lock().await;
        let set: JwkSet = self
            .client
            .http()
            .get(self.client.url("/oidc/jwks"))
            .timeout(FETCH_TIMEOUT)
            .send()
            .await
            .and_then(|r| r.error_for_status())
            .map_err(|e| VerifyError::Unavailable(e.to_string()))?
            .json()
            .await
            .map_err(|e| VerifyError::Unavailable(e.to_string()))?;
        self.keys.store(Arc::new(Keys {
            verifier: pacod_id_jose::Verifier::from_jwks(&set),
            read_at: Some(Instant::now()),
        }));
        Ok(())
    }
}

impl Verifier {
    pub fn new(client: IdClient) -> Self {
        Verifier {
            inner: Arc::new(Inner {
                client,
                keys: ArcSwap::from_pointee(Keys { verifier: pacod_id_jose::Verifier::default(), read_at: None }),
                reading: tokio::sync::Mutex::new(()),
                tried_at: std::sync::Mutex::new(None),
                refresh_every: REFRESH_EVERY,
            }),
        }
    }

    /// How often known keys are read again (one hour by default). Shorter means a key pacod-id
    /// revoked (`pacod-id keys rotate --revoke`) stops verifying sooner.
    pub fn with_refresh_every(client: IdClient, every: Duration) -> Self {
        let v = Self::new(client);
        let inner = Arc::into_inner(v.inner).expect("just made");
        Verifier { inner: Arc::new(Inner { refresh_every: every, ..inner }) }
    }

    /// Reads the keys now (also done by itself when an unknown key id shows up).
    pub async fn refresh_keys(&self) -> Result<(), VerifyError> {
        *self.inner.tried_at.lock().expect("not poisoned") = Some(Instant::now());
        self.inner.read().await
    }

    async fn keys_for(&self, kid: &str) -> Result<Arc<Keys>, VerifyError> {
        let keys = self.inner.keys.load_full();
        if keys.verifier.has_kid(kid) {
            // Known: answer with these, and read newer ones in the background when due. A
            // pacod-id that does not answer never holds up a request here.
            if keys.read_at.is_some_and(|t| t.elapsed() >= self.inner.refresh_every) && self.inner.claim_read() {
                let inner = self.inner.clone();
                tokio::spawn(async move {
                    if let Err(e) = inner.read().await {
                        tracing::warn!(error = %e, "reading pacod-id's keys failed; the known ones stay");
                    }
                });
            }
            return Ok(keys);
        }
        // An unknown key id (pacod-id rotated, or nothing read yet): one read at a time, at most
        // every REFETCH_AT_MOST; whoever comes while one runs waits for it instead of starting
        // another.
        if self.inner.claim_read() {
            if let Err(e) = self.inner.read().await
                && keys.read_at.is_none()
            {
                return Err(e);
            }
        } else {
            drop(self.inner.reading.lock().await);
        }
        let now = self.inner.keys.load_full();
        if now.read_at.is_none() {
            return Err(VerifyError::Unavailable("no keys read yet".into()));
        }
        Ok(now)
    }

    /// Checks a bearer token. Signature first, then the claims.
    pub async fn verify(&self, token: &str) -> Result<AccessToken, VerifyError> {
        let invalid = |why: &str| VerifyError::Invalid(why.to_owned());
        let header = pacod_id_jose::decode_header(token).map_err(|e| VerifyError::Invalid(e.to_string()))?;
        let keys = self.keys_for(&header.kid).await?;
        let v =
            keys.verifier.verify(token, &[Alg::RS256, Alg::ES256]).map_err(|e| VerifyError::Invalid(e.to_string()))?;
        if v.header.typ.as_deref() != Some("at+jwt") {
            return Err(invalid("not an access token"));
        }
        let c = &v.claims;
        let config = self.inner.client.config();
        let issuer = c["iss"]
            .as_str()
            .filter(|i| config.issuers.iter().any(|x| x == i))
            .ok_or_else(|| invalid("another issuer"))?;
        let aud_ok = match &c["aud"] {
            Value::String(a) => *a == config.audience,
            Value::Array(list) => list.iter().any(|a| *a == config.audience.as_str()),
            _ => false,
        };
        if !aud_ok {
            return Err(invalid("for another audience"));
        }
        let now = crate::now();
        let exp = c["exp"].as_i64().ok_or_else(|| invalid("no expiry"))?;
        if exp + LEEWAY <= now {
            return Err(VerifyError::Expired);
        }
        if c["iat"].as_i64().is_some_and(|i| i > now + LEEWAY) || c["nbf"].as_i64().is_some_and(|n| n > now + LEEWAY) {
            return Err(invalid("not valid yet"));
        }
        let kind = c["pid_kind"]
            .as_str()
            .filter(|k| *k == "user" || *k == "service")
            .ok_or_else(|| invalid("no subject kind"))?;
        let sid = c["sid"].as_str().map(str::to_owned);
        if kind == "user" && sid.is_none() {
            return Err(invalid("a person's token without a session"));
        }
        Ok(AccessToken {
            sub: c["sub"].as_str().filter(|s| !s.is_empty()).ok_or_else(|| invalid("no subject"))?.to_owned(),
            kind: kind.to_owned(),
            sid,
            client_id: c["client_id"].as_str().unwrap_or_default().to_owned(),
            scope: c["scope"].as_str().unwrap_or_default().to_owned(),
            issuer: issuer.to_owned(),
            actor: c["act"]["sub"].as_str().map(str::to_owned),
            expires_at: exp,
            email: c["email"].as_str().map(str::to_owned),
            name: c["name"].as_str().map(str::to_owned),
        })
    }
}
