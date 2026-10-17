//! The internal API, with the service's client credentials.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use pacod_id_authz::ManifestSource;
use serde::Deserialize;
use serde_json::{Value, json};

/// Where and as whom a product's service talks to pacod-id.
#[derive(Debug, Clone)]
pub struct IdConfig {
    /// The public issuers whose access tokens the product takes (one per realm it serves),
    /// exactly as their `iss` claims carry them.
    pub issuers: Vec<String>,
    /// pacod-id's internal listener as the service reaches it (`http://id:8071`): JWKS, token,
    /// introspection, the feed and the admin API all go there, never to the public address.
    pub internal_url: String,
    /// The service's client (kind `service` in pacod-id's configuration) and its secret.
    pub client_id: String,
    pub client_secret: String,
    /// The product (`gw`): the feed and the catalogue are its own.
    pub product: String,
    /// The audience of the access tokens the product takes (`pacod-gw`).
    pub audience: String,
}

/// An answer of pacod-id that is not a success, or no answer.
#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("pacod-id cannot be reached: {0}")]
    Unreachable(#[from] reqwest::Error),
    /// pacod-id's error code (`details.code`, or the OAuth `error`), with its HTTP status.
    #[error("pacod-id answered {status}: {code}: {message}")]
    Refused { status: u16, code: String, message: String },
    #[error("pacod-id's answer does not read: {0}")]
    Malformed(String),
}

impl ApiError {
    /// The error code, when pacod-id refused.
    pub fn code(&self) -> Option<&str> {
        match self {
            ApiError::Refused { code, .. } => Some(code),
            _ => None,
        }
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            ApiError::Refused { status, .. } => Some(*status),
            _ => None,
        }
    }
}

struct Cached {
    token: String,
    until: Instant,
}

struct Inner {
    config: IdConfig,
    http: reqwest::Client,
    service_token: tokio::sync::Mutex<Option<Cached>>,
}

/// The internal API. Cheap to clone; one per process is enough.
#[derive(Clone)]
pub struct IdClient {
    inner: Arc<Inner>,
}

/// Longer than the feed's longest wait (25 s) with room to spare.
const TIMEOUT: Duration = Duration::from_secs(40);

async fn read<T: serde::de::DeserializeOwned>(r: reqwest::Response) -> Result<T, ApiError> {
    let status = r.status().as_u16();
    let body: Value = r.json().await.unwrap_or(Value::Null);
    if (200..300).contains(&status) {
        return serde_json::from_value(body).map_err(|e| ApiError::Malformed(e.to_string()));
    }
    let code = body["error"]["details"]["code"]
        .as_str()
        .or_else(|| body["error"]["type"].as_str())
        .or_else(|| body["error"].as_str())
        .unwrap_or("error")
        .to_owned();
    let message =
        body["error"]["message"].as_str().or_else(|| body["error_description"].as_str()).unwrap_or("").to_owned();
    Err(ApiError::Refused { status, code, message })
}

/// Tokens from the token endpoint.
#[derive(Debug, Clone, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
}

/// What a catalogue push did.
#[derive(Debug, Clone, Deserialize)]
pub struct Pushed {
    pub version: i64,
    pub changed: bool,
    pub seq: i64,
}

#[derive(Deserialize)]
struct Seq {
    seq: i64,
}

impl IdClient {
    pub fn new(config: IdConfig) -> Self {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("an HTTP client");
        IdClient { inner: Arc::new(Inner { config, http, service_token: tokio::sync::Mutex::new(None) }) }
    }

    pub fn config(&self) -> &IdConfig {
        &self.inner.config
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    pub(crate) fn url(&self, path: &str) -> String {
        format!("{}{path}", self.inner.config.internal_url.trim_end_matches('/'))
    }

    fn token_request(&self) -> reqwest::RequestBuilder {
        let c = &self.inner.config;
        self.inner
            .http
            .post(self.url("/oidc/token"))
            .basic_auth(form_encode(&c.client_id), Some(form_encode(&c.client_secret)))
    }

    /// The service's own access token (client credentials, `aud` pacod-id), cached until 30
    /// seconds before it expires.
    pub async fn service_token(&self) -> Result<String, ApiError> {
        let mut cached = self.inner.service_token.lock().await;
        if let Some(c) = cached.as_ref().filter(|c| c.until > Instant::now()) {
            return Ok(c.token.clone());
        }
        let t: Tokens = read(self.token_request().form(&[("grant_type", "client_credentials")]).send().await?).await?;
        let life = Duration::from_secs(t.expires_in.unwrap_or(300).saturating_sub(30).max(5));
        *cached = Some(Cached { token: t.access_token.clone(), until: Instant::now() + life });
        Ok(t.access_token)
    }

    /// Sends a request with the service's token; when pacod-id refuses the cached one (its
    /// keys rotated at once, say), once more with a new token rather than failing until the
    /// cached one expires.
    async fn send_as_service(
        &self,
        make: impl Fn(&str) -> reqwest::RequestBuilder,
    ) -> Result<reqwest::Response, ApiError> {
        let token = self.service_token().await?;
        let r = make(&token).send().await?;
        if r.status() != reqwest::StatusCode::UNAUTHORIZED {
            return Ok(r);
        }
        *self.inner.service_token.lock().await = None;
        let token = self.service_token().await?;
        Ok(make(&token).send().await?)
    }

    async fn call<T: serde::de::DeserializeOwned>(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<Value>,
    ) -> Result<T, ApiError> {
        let r = self
            .send_as_service(|token| {
                let r = self.inner.http.request(method.clone(), self.url(path)).bearer_auth(token);
                if let Some(b) = &body { r.json(b) } else { r }
            })
            .await?;
        read(r).await
    }

    // The catalogue, organisations and scopes ------------------------------

    /// Pushes the product's manifest; `migrate` says what becomes of bindings of dropped roles.
    pub async fn push_catalog(
        &self,
        manifest: &ManifestSource,
        migrate: &HashMap<String, Option<String>>,
    ) -> Result<Pushed, ApiError> {
        let path = format!("/internal/v1/catalog/{}", self.inner.config.product);
        self.call(reqwest::Method::PUT, &path, Some(json!({ "manifest": manifest, "migrate": migrate }))).await
    }

    /// Creates or renames an organisation and registers the product with it. The feed's `seq`.
    pub async fn put_organisation(
        &self,
        id: &str,
        slug: &str,
        display_name: &str,
        system: bool,
    ) -> Result<i64, ApiError> {
        let body = json!({ "slug": slug, "display_name": display_name, "system": system });
        Ok(self.call::<Seq>(reqwest::Method::PUT, &format!("/internal/v1/organisations/{id}"), Some(body)).await?.seq)
    }

    /// Unregisters the product; the organisation goes when no product has it.
    pub async fn delete_organisation(&self, id: &str) -> Result<i64, ApiError> {
        Ok(self.call::<Seq>(reqwest::Method::DELETE, &format!("/internal/v1/organisations/{id}"), None).await?.seq)
    }

    /// Registers a product scope (`gw.workspace`) under its parent (`organisation:<id>`).
    pub async fn put_scope(
        &self,
        kind: &str,
        id: &str,
        parent: &str,
        display_name: &str,
        system: bool,
    ) -> Result<i64, ApiError> {
        let body = json!({ "parent": parent, "display_name": display_name, "system": system });
        Ok(self.call::<Seq>(reqwest::Method::PUT, &format!("/internal/v1/scopes/{kind}/{id}"), Some(body)).await?.seq)
    }

    pub async fn delete_scope(&self, kind: &str, id: &str) -> Result<i64, ApiError> {
        Ok(self.call::<Seq>(reqwest::Method::DELETE, &format!("/internal/v1/scopes/{kind}/{id}"), None).await?.seq)
    }

    // The feed --------------------------------------------------------------------

    pub(crate) async fn snapshot_raw(&self) -> Result<Value, ApiError> {
        self.call(
            reqwest::Method::GET,
            &format!("/internal/v1/authz/snapshot?product={}", self.inner.config.product),
            None,
        )
        .await
    }

    pub(crate) async fn changes_raw(&self, after: i64, wait: u64) -> Result<Value, ApiError> {
        let path =
            format!("/internal/v1/authz/changes?product={}&after={after}&wait={wait}", self.inner.config.product);
        self.call(reqwest::Method::GET, &path, None).await
    }

    /// The product has deleted its data about a deleted account.
    pub async fn deletion_done(&self, sub: &str) -> Result<(), ApiError> {
        let path = format!("/internal/v1/deletions/{}/done?product={}", segment(sub), self.inner.config.product);
        let r = self.send_as_service(|token| self.inner.http.post(self.url(&path)).bearer_auth(token)).await?;
        if r.status().is_success() { Ok(()) } else { read::<Value>(r).await.map(|_| ()) }
    }

    /// The product's last import and the identity writes at pacod-id since it,
    /// as `GET /internal/v1/imports/{product}` answers; `None` when it was never imported.
    pub async fn last_import(&self) -> Result<Option<Value>, ApiError> {
        let path = format!("/internal/v1/imports/{}", self.inner.config.product);
        match self.call::<Value>(reqwest::Method::GET, &path, None).await {
            Ok(v) => Ok(Some(v)),
            Err(e) if e.code() == Some("no_import") => Ok(None),
            Err(e) => Err(e),
        }
    }

    // Sessions and tokens -------------------------------------------------------------------------

    /// Ends a session server to server (a product's sign-out).
    pub async fn revoke_session(&self, sid: &str) -> Result<(), ApiError> {
        let path = format!("/internal/v1/sessions/{}/revoke", segment(sid));
        let r = self.send_as_service(|token| self.inner.http.post(self.url(&path)).bearer_auth(token)).await?;
        if r.status().is_success() { Ok(()) } else { read::<Value>(r).await.map(|_| ()) }
    }

    pub(crate) async fn introspect_raw(&self, token: &str) -> Result<Value, ApiError> {
        let r = self
            .send_as_service(|service| {
                self.inner.http.post(self.url("/internal/v1/introspect")).bearer_auth(service).form(&[("token", token)])
            })
            .await?;
        read(r).await
    }

    /// Swaps a caller's token (a person's access token for this product, or a service token) for
    /// a 60-second token for pacod-id's admin API, acting for the caller.
    pub async fn exchange_for_admin(&self, subject_token: &str) -> Result<Tokens, ApiError> {
        let form = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:token-exchange"),
            ("subject_token", subject_token),
            ("subject_token_type", "urn:ietf:params:oauth:token-type:access_token"),
            ("audience", "pacod-id"),
        ];
        read(self.token_request().form(&form).send().await?).await
    }

    /// A delegated grant for a long task acting for a person: a refresh token bound to the
    /// person's session (it ends with it), with an access token for the product.
    pub async fn delegated_grant(&self, subject_token: &str) -> Result<Tokens, ApiError> {
        let form = [
            ("grant_type", "urn:ietf:params:oauth:grant-type:token-exchange"),
            ("subject_token", subject_token),
            ("subject_token_type", "urn:ietf:params:oauth:token-type:access_token"),
            ("requested_token_type", "urn:ietf:params:oauth:token-type:refresh_token"),
        ];
        read(self.token_request().form(&form).send().await?).await
    }

    /// New tokens for a refresh token of this client (a delegated grant).
    pub async fn refresh(&self, refresh_token: &str) -> Result<Tokens, ApiError> {
        read(
            self.token_request()
                .form(&[("grant_type", "refresh_token"), ("refresh_token", refresh_token)])
                .send()
                .await?,
        )
        .await
    }

    /// Revokes a refresh token of this client (RFC 7009).
    pub async fn revoke(&self, refresh_token: &str) -> Result<(), ApiError> {
        let c = &self.inner.config;
        let r = self
            .inner
            .http
            .post(self.url("/oidc/revoke"))
            .basic_auth(form_encode(&c.client_id), Some(form_encode(&c.client_secret)))
            .form(&[("token", refresh_token)])
            .send()
            .await?;
        if r.status().is_success() { Ok(()) } else { read::<Value>(r).await.map(|_| ()) }
    }

    /// A request to pacod-id's admin API (`/admin/v1/...`) with a token from
    /// [`IdClient::exchange_for_admin`].
    pub fn admin(&self, method: reqwest::Method, path: &str, facade_token: &str) -> reqwest::RequestBuilder {
        self.inner.http.request(method, self.url(&format!("/admin/v1{path}"))).bearer_auth(facade_token)
    }

    /// Reads an answer of the admin API.
    pub async fn read<T: serde::de::DeserializeOwned>(r: reqwest::Response) -> Result<T, ApiError> {
        read(r).await
    }
}

/// HTTP Basic credentials are form-encoded first (RFC 6749, 2.3.1).
/// A path segment: anything but unreserved characters percent-encoded.
fn segment(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn form_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'*' => (b as char).to_string(),
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Facade tokens, cached per caller token until shortly before they expire.
pub struct TokenExchanger {
    client: IdClient,
    cache: Mutex<HashMap<[u8; 32], (String, Instant)>>,
}

/// The most cached facade tokens; past it the cache starts again.
const EXCHANGE_CACHE: usize = 4096;

impl TokenExchanger {
    pub fn new(client: IdClient) -> Self {
        TokenExchanger { client, cache: Mutex::new(HashMap::new()) }
    }

    /// A token for pacod-id's admin API acting for the caller of `subject_token`.
    pub async fn for_caller(&self, subject_token: &str) -> Result<String, ApiError> {
        let key = crate::sha256(subject_token.as_bytes());
        if let Some((t, until)) = self.cache.lock().expect("lock").get(&key)
            && *until > Instant::now()
        {
            return Ok(t.clone());
        }
        let t = self.client.exchange_for_admin(subject_token).await?;
        let life = Duration::from_secs(t.expires_in.unwrap_or(60).saturating_sub(10).max(1));
        let mut cache = self.cache.lock().expect("lock");
        if cache.len() >= EXCHANGE_CACHE {
            cache.clear();
        }
        cache.insert(key, (t.access_token.clone(), Instant::now() + life));
        Ok(t.access_token)
    }
}
