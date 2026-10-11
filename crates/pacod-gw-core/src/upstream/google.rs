//! Access tokens for Google service accounts, for Vertex AI.
//!
//! A credential with the scheme `google_service_account` holds a service account's JSON key.
//! The gateway signs a JWT with the key (RS256) and exchanges it at Google's token endpoint for
//! an access token that lasts an hour
//! (<https://developers.google.com/identity/protocols/oauth2/service-account>). Tokens are kept
//! per credential and minted again five minutes before they expire, one mint at a time per
//! credential; a failed mint is remembered for half a minute so that a broken key does not
//! send every call to Google.
//!
//! The token endpoint is fixed; the key file's `token_uri` is not used. Only the installation
//! can point it elsewhere (`PGW_GOOGLE_TOKEN_URL`), for tests. Keys come from administrators of
//! organisations and workspaces too, so they are read strictly. Neither a key, an assertion nor
//! a token is logged.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use bytes::Bytes;
use http::{HeaderValue, Method, Request};
use http_body_util::{BodyExt, Full, Limited};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::Upstream;

/// Google's token endpoint, also the audience of every assertion.
pub const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// What a token may do: Vertex AI needs the cloud-platform scope.
pub const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
/// A token is minted again when less than this is left of it.
const MARGIN: Duration = Duration::from_secs(5 * 60);
/// How long a failed mint is answered from memory.
const FAILURE_MEMORY: Duration = Duration::from_secs(30);
/// The largest key file read.
const MAX_KEY_BYTES: usize = 16 << 10;
/// Above this many cached credentials, those without a usable token are dropped.
const PRUNE_AT: usize = 256;

/// The parts of a service-account key that signing needs.
pub struct ServiceAccount {
    pub client_email: String,
    pub private_key_id: String,
    key: RsaKeyPair,
}

impl std::fmt::Debug for ServiceAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceAccount").field("client_email", &self.client_email).finish_non_exhaustive()
    }
}

impl ServiceAccount {
    /// Reads a key file's JSON, strictly: a service account of Google's own universe with an
    /// RSA key of at least 2048 bits. The message says what is wrong, never what the key holds.
    pub fn parse(text: &str) -> Result<Self, String> {
        if text.len() > MAX_KEY_BYTES {
            return Err("the service-account key is larger than 16 KiB".into());
        }
        let v: Value = serde_json::from_str(text).map_err(|_| "the service-account key is not JSON".to_owned())?;
        if v["type"] != "service_account" {
            return Err("the key is not a service-account key (its type is not \"service_account\")".into());
        }
        if v.get("universe_domain").is_some_and(|u| u != "googleapis.com") {
            return Err("the service account is not of Google Cloud's own universe (googleapis.com)".into());
        }
        let field = |name: &str| v[name].as_str().filter(|s| !s.is_empty()).map(str::to_owned);
        let client_email = field("client_email")
            .filter(|e| e.ends_with(".iam.gserviceaccount.com") && e.contains('@'))
            .ok_or("client_email is missing or not a service account's address")?;
        let private_key_id = field("private_key_id")
            .filter(|id| id.len() == 40 && id.bytes().all(|b| b.is_ascii_hexdigit()))
            .ok_or("private_key_id is missing or not 40 hexadecimal digits")?;
        let pem = field("private_key").ok_or("private_key is missing")?;
        let der = pem_der(&pem)?;
        let key = RsaKeyPair::from_pkcs8(&der).map_err(|_| "the private key is not a usable RSA key".to_owned())?;
        if key.public().modulus_len() < 256 {
            return Err("the private key is shorter than 2048 bits".into());
        }
        Ok(Self { client_email, private_key_id, key })
    }

    /// The signed JWT that asks for a token valid for an hour from `now` (seconds since the
    /// epoch), addressed to `audience`.
    pub fn assertion(&self, now: u64, audience: &str) -> Result<Zeroizing<String>, String> {
        let header = json!({ "alg": "RS256", "typ": "JWT", "kid": self.private_key_id });
        let claims =
            json!({ "iss": self.client_email, "scope": SCOPE, "aud": audience, "iat": now, "exp": now + 3600 });
        let signing_input =
            format!("{}.{}", URL_SAFE_NO_PAD.encode(header.to_string()), URL_SAFE_NO_PAD.encode(claims.to_string()));
        let mut signature = vec![0; self.key.public().modulus_len()];
        self.key
            .sign(&RSA_PKCS1_SHA256, &SystemRandom::new(), signing_input.as_bytes(), &mut signature)
            .map_err(|_| "signing the assertion failed".to_owned())?;
        Ok(Zeroizing::new(format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(signature))))
    }

    /// What a credential list shows for the key: the account and the start of its key id.
    pub fn fingerprint(&self) -> String {
        format!("{} (key {})", self.client_email, &self.private_key_id[..8])
    }
}

/// Whether a secret looks like a service-account key file rather than an API key.
pub fn looks_like_service_account(text: &str) -> bool {
    let t = text.trim_start();
    t.starts_with('{') && (t.contains("\"service_account\"") || t.contains("\"private_key\""))
}

/// The DER bytes of a PEM `PRIVATE KEY` block (PKCS#8).
fn pem_der(pem: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let begin = pem.find("-----BEGIN PRIVATE KEY-----").ok_or("the private key is not a PKCS#8 PEM block")?;
    let end = pem.find("-----END PRIVATE KEY-----").ok_or("the private key's PEM block has no end")?;
    let body: Zeroizing<String> = Zeroizing::new(
        pem[begin + "-----BEGIN PRIVATE KEY-----".len()..end].chars().filter(|c| !c.is_whitespace()).collect(),
    );
    STANDARD.decode(body.as_bytes()).map(Zeroizing::new).map_err(|_| "the private key's PEM is not base64".to_owned())
}

/// A token and when it stops being usable.
struct Minted {
    token: Zeroizing<String>,
    expires: Instant,
}

/// One credential's token, with the lock that lets one mint run at a time.
#[derive(Default)]
struct Slot {
    current: parking_lot::RwLock<Option<Minted>>,
    failed: parking_lot::Mutex<Option<(Instant, String)>>,
    minting: tokio::sync::Mutex<()>,
}

impl Slot {
    fn usable(&self) -> Option<Zeroizing<String>> {
        let current = self.current.read();
        current.as_ref().filter(|m| m.expires > Instant::now() + MARGIN).map(|m| m.token.clone())
    }
}

/// The tokens of every service-account credential this instance has used.
pub struct GoogleTokens {
    url: String,
    slots: parking_lot::Mutex<HashMap<String, Arc<Slot>>>,
    /// How many tokens were minted, for tests and the curious.
    pub minted: std::sync::atomic::AtomicU64,
}

impl GoogleTokens {
    /// `url` overrides Google's token endpoint (`PGW_GOOGLE_TOKEN_URL`, for tests).
    pub fn new(url: Option<String>) -> Self {
        Self {
            url: url.filter(|u| !u.is_empty()).unwrap_or_else(|| TOKEN_URL.to_owned()),
            slots: parking_lot::Mutex::new(HashMap::new()),
            minted: std::sync::atomic::AtomicU64::new(0),
        }
    }

    fn slot(&self, credential_id: &str) -> Arc<Slot> {
        let mut slots = self.slots.lock();
        if slots.len() > PRUNE_AT {
            slots.retain(|_, s| s.usable().is_some());
        }
        slots.entry(credential_id.to_owned()).or_default().clone()
    }

    /// A usable access token for the credential `credential_id`, whose secret is `key` (the
    /// JSON key file). Minted when there is none or it runs out within five minutes.
    pub async fn token(
        &self,
        upstream: &Upstream,
        credential_id: &str,
        key: &str,
    ) -> Result<Zeroizing<String>, String> {
        let slot = self.slot(credential_id);
        if let Some(token) = slot.usable() {
            return Ok(token);
        }
        let _one_at_a_time = slot.minting.lock().await;
        // Another call may have minted while this one waited.
        if let Some(token) = slot.usable() {
            return Ok(token);
        }
        if let Some((until, why)) = slot.failed.lock().as_ref()
            && *until > Instant::now()
        {
            return Err(why.clone());
        }
        match self.mint(upstream, key).await {
            Ok(minted) => {
                let token = minted.token.clone();
                *slot.current.write() = Some(minted);
                *slot.failed.lock() = None;
                self.minted.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Ok(token)
            }
            Err(why) => {
                *slot.failed.lock() = Some((Instant::now() + FAILURE_MEMORY, why.clone()));
                Err(why)
            }
        }
    }

    /// Forgets the token of `credential_id` if it is still `token`: the provider refused it.
    pub fn invalidate(&self, credential_id: &str, token: &str) {
        let slot = self.slots.lock().get(credential_id).cloned();
        if let Some(slot) = slot {
            let mut current = slot.current.write();
            if current.as_ref().is_some_and(|m| m.token.as_str() == token) {
                *current = None;
            }
        }
    }

    async fn mint(&self, upstream: &Upstream, key: &str) -> Result<Minted, String> {
        let account = ServiceAccount::parse(key)?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "the clock is before 1970".to_owned())?;
        let assertion = account.assertion(now.as_secs(), TOKEN_URL)?;
        let form = Zeroizing::new(format!(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={}",
            assertion.as_str()
        ));
        let req = Request::builder()
            .method(Method::POST)
            .uri(&self.url)
            .header("content-type", HeaderValue::from_static("application/x-www-form-urlencoded"))
            .body(axum::body::Body::new(Full::new(Bytes::from(form.as_bytes().to_vec()))))
            .map_err(|_| "the token endpoint's address is not valid".to_owned())?;
        let started = Instant::now();
        let sent = upstream
            .send(req, Duration::from_secs(10))
            .await
            .map_err(|e| format!("Google's token endpoint could not be reached: {e}"))?;
        let status = sent.response.status();
        let body = Limited::new(sent.response.into_body(), 64 << 10)
            .collect()
            .await
            .map_err(|_| "Google's token endpoint did not finish its answer".to_owned())?
            .to_bytes();
        let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
        match (status.is_success(), v["access_token"].as_str()) {
            (true, Some(token)) if !token.is_empty() => {
                let lifetime = v["expires_in"].as_u64().unwrap_or(3600).clamp(60, 3600);
                Ok(Minted { token: Zeroizing::new(token.to_owned()), expires: started + Duration::from_secs(lifetime) })
            }
            // Google's error names say what is wrong (invalid_grant: a deleted or disabled key,
            // a wrong clock) and hold nothing secret.
            _ => Err(format!(
                "Google's token endpoint refused the service account ({}: {})",
                status.as_u16(),
                v["error"].as_str().unwrap_or("no token in the answer")
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

    const KEY: &str = pacod_gw_testkit::google::TEST_SERVICE_ACCOUNT;

    #[test]
    fn the_assertion_is_signed_and_the_same_as_the_testkits() {
        let account = ServiceAccount::parse(KEY).unwrap();
        let jwt = account.assertion(1_800_000_000, TOKEN_URL).unwrap();
        let signed = &jwt[..jwt.rfind('.').unwrap()];
        let signature = URL_SAFE_NO_PAD.decode(&jwt[jwt.rfind('.').unwrap() + 1..]).unwrap();
        UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, account.key.public().as_ref())
            .verify(signed.as_bytes(), &signature)
            .expect("the signature checks out");
        // The testkit signs the same way, so a recording proves what the gateway sends.
        let other = pacod_gw_testkit::google::ServiceAccount::parse(KEY).unwrap();
        assert_eq!(jwt.as_str(), other.assertion(1_800_000_000, TOKEN_URL).unwrap());
        assert_eq!(account.fingerprint(), format!("{} (key {})", account.client_email, &account.private_key_id[..8]));
    }

    #[test]
    fn only_a_real_service_account_key_is_taken() {
        let base: Value = serde_json::from_str(KEY).unwrap();
        let with = |k: &str, v: Value| {
            let mut x = base.clone();
            x[k] = v;
            x.to_string()
        };
        for (field, value, why) in [
            ("type", json!("authorized_user"), "type"),
            ("client_email", json!("someone@example.com"), "client_email"),
            ("private_key_id", json!("abc"), "private_key_id"),
            ("private_key", json!("-----BEGIN RSA PRIVATE KEY-----\nAAAA\n-----END RSA PRIVATE KEY-----\n"), "PKCS#8"),
            ("private_key", json!("-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n"), "usable"),
            ("universe_domain", json!("example.com"), "universe"),
        ] {
            let err = ServiceAccount::parse(&with(field, value)).unwrap_err();
            assert!(err.contains(why), "{field}: {err}");
            assert!(!err.contains("BEGIN"), "never the key itself");
        }
        assert!(ServiceAccount::parse(&"x".repeat(20_000)).unwrap_err().contains("16 KiB"));
        assert!(looks_like_service_account(KEY));
        assert!(!looks_like_service_account("sk-abc"));
    }
}
