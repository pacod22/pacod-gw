//! A small OpenID provider that stands in for Google where Google cannot be used:
//! pacod-id's tests, the development stack, and fixture apps. Authorization code with PKCE, an
//! RS256 id_token with `nonce`, `email` and `email_verified`, discovery and JWKS.
//!
//! Who signs in is set by the test (`queue`) or, in a browser, with a small form. It checks
//! client secrets, redirect URIs it saw at authorize, PKCE and single use of codes, so a relying
//! party that skips one of them fails against it as it would against Google.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Form, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD as B64};
use pacod_id_jose::{Alg, JwkSet, SigningKey};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Deserialize;
use serde_json::json;

/// Who signs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub sub: String,
    pub email: String,
    pub email_verified: bool,
    pub name: String,
}

impl Identity {
    pub fn new(sub: &str, email: &str, email_verified: bool) -> Self {
        Identity {
            sub: sub.into(),
            email: email.into(),
            email_verified,
            name: email.split('@').next().unwrap_or("").into(),
        }
    }
}

struct Pending {
    identity: Identity,
    client_id: String,
    redirect_uri: String,
    nonce: Option<String>,
    challenge: Option<String>,
    issued: Instant,
}

struct Inner {
    issuer: String,
    key: SigningKey,
    clients: BTreeMap<String, String>,
    next: Mutex<VecDeque<Option<Identity>>>,
    codes: Mutex<HashMap<String, Pending>>,
    /// Authorize requests seen, for tests to look at.
    seen: Mutex<Vec<BTreeMap<String, String>>>,
    /// Claims laid over the next id_token, for tests of what a relying party must refuse.
    tamper: Mutex<Option<serde_json::Value>>,
}

/// The provider. Cheap to clone.
#[derive(Clone)]
pub struct MockIdp {
    inner: Arc<Inner>,
}

fn random() -> String {
    let mut b = [0u8; 24];
    SystemRandom::new().fill(&mut b).expect("random bytes");
    B64.encode(b)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

impl MockIdp {
    /// A provider for `issuer` (its public URL) with these clients (id → secret).
    pub fn new(issuer: &str, clients: &[(&str, &str)]) -> anyhow::Result<Self> {
        use rsa::pkcs8::EncodePrivateKey;
        let rsa = rsa::RsaPrivateKey::new(&mut rsa::rand_core::OsRng, 2048)?;
        let der = rsa.to_pkcs8_der()?;
        let key = SigningKey::from_pkcs8(format!("mock-{}", &random()[..8]), Alg::RS256, der.as_bytes())?;
        Ok(MockIdp {
            inner: Arc::new(Inner {
                issuer: issuer.trim_end_matches('/').to_owned(),
                key,
                clients: clients.iter().map(|(i, s)| ((*i).to_owned(), (*s).to_owned())).collect(),
                next: Mutex::new(VecDeque::new()),
                codes: Mutex::new(HashMap::new()),
                seen: Mutex::new(Vec::new()),
                tamper: Mutex::new(None),
            }),
        })
    }

    pub fn issuer(&self) -> &str {
        &self.inner.issuer
    }

    /// The next authorize request signs this identity in at once.
    pub fn queue(&self, identity: Identity) {
        self.inner.next.lock().expect("lock").push_back(Some(identity));
    }

    /// The next authorize request answers `access_denied`, as when someone cancels at Google.
    pub fn deny_next(&self) {
        self.inner.next.lock().expect("lock").push_back(None);
    }

    /// Lays these claims over the next id_token (`null` removes one).
    pub fn tamper_next(&self, claims: serde_json::Value) {
        *self.inner.tamper.lock().expect("lock") = Some(claims);
    }

    /// The authorize requests seen so far.
    pub fn seen(&self) -> Vec<BTreeMap<String, String>> {
        self.inner.seen.lock().expect("lock").clone()
    }

    /// A token as the provider signs them, with any claims (for tests of bad tokens).
    pub fn sign(&self, claims: &serde_json::Value) -> String {
        self.inner.key.sign("JWT", claims).expect("signing")
    }

    pub fn router(&self) -> Router {
        Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/jwks", get(jwks))
            .route("/authorize", get(authorize).post(authorize_form))
            .route("/token", axum::routing::post(token))
            .with_state(self.clone())
    }

    /// Serves until the process ends.
    pub async fn serve(self, listener: tokio::net::TcpListener) -> std::io::Result<()> {
        axum::serve(listener, self.router()).await
    }
}

async fn discovery(State(m): State<MockIdp>) -> Json<serde_json::Value> {
    let i = &m.inner.issuer;
    Json(json!({
        "issuer": i,
        "authorization_endpoint": format!("{i}/authorize"),
        "token_endpoint": format!("{i}/token"),
        "jwks_uri": format!("{i}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "scopes_supported": ["openid", "email", "profile"],
        "token_endpoint_auth_methods_supported": ["client_secret_basic", "client_secret_post"],
        "code_challenge_methods_supported": ["S256"],
    }))
}

async fn jwks(State(m): State<MockIdp>) -> Json<JwkSet> {
    Json(JwkSet { keys: vec![m.inner.key.public_jwk()] })
}

fn bad(status: StatusCode, error: &str) -> Response {
    (status, Json(json!({ "error": error }))).into_response()
}

fn with_query(base: &str, params: &[(&str, &str)]) -> String {
    let mut u = url::Url::parse(base).expect("a checked URL");
    u.query_pairs_mut().extend_pairs(params);
    u.into()
}

/// Issues a code for `identity` and sends the browser back.
fn grant(m: &MockIdp, q: &BTreeMap<String, String>, identity: Identity) -> Response {
    let code = random();
    m.inner.codes.lock().expect("lock").insert(
        code.clone(),
        Pending {
            identity,
            client_id: q.get("client_id").cloned().unwrap_or_default(),
            redirect_uri: q.get("redirect_uri").cloned().unwrap_or_default(),
            nonce: q.get("nonce").cloned(),
            challenge: q.get("code_challenge").cloned(),
            issued: Instant::now(),
        },
    );
    let mut params = vec![("code", code.as_str())];
    if let Some(s) = q.get("state") {
        params.push(("state", s));
    }
    Redirect::to(&with_query(&q["redirect_uri"], &params)).into_response()
}

/// Why a request is refused, if it is.
fn refused(m: &MockIdp, q: &BTreeMap<String, String>) -> Option<Response> {
    if !q.get("client_id").is_some_and(|c| m.inner.clients.contains_key(c)) {
        return Some(bad(StatusCode::BAD_REQUEST, "invalid_client"));
    }
    if !q.get("redirect_uri").is_some_and(|r| url::Url::parse(r).is_ok())
        || q.get("response_type").map(String::as_str) != Some("code")
    {
        return Some(bad(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    if q.get("code_challenge_method").map(String::as_str) != Some("S256") || !q.contains_key("code_challenge") {
        return Some(bad(StatusCode::BAD_REQUEST, "invalid_request"));
    }
    None
}

async fn authorize(State(m): State<MockIdp>, Query(q): Query<BTreeMap<String, String>>) -> Response {
    if let Some(r) = refused(&m, &q) {
        return r;
    }
    m.inner.seen.lock().expect("lock").push(q.clone());
    let next = m.inner.next.lock().expect("lock").pop_front();
    match next {
        Some(Some(identity)) => grant(&m, &q, identity),
        Some(None) => {
            let mut params = vec![("error", "access_denied")];
            if let Some(s) = q.get("state") {
                params.push(("state", s));
            }
            Redirect::to(&with_query(&q["redirect_uri"], &params)).into_response()
        }
        None => {
            // A browser: ask who signs in.
            let hidden: String = q
                .iter()
                .map(|(k, v)| format!("<input type=\"hidden\" name=\"{}\" value=\"{}\">", escape(k), escape(v)))
                .collect();
            Html(format!(
                "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><title>Mock sign-in</title></head>\
                 <body style=\"font-family:system-ui,sans-serif;max-width:28rem;margin:3rem auto\"><h1>Mock Google</h1>\
                 <form method=\"post\" action=\"/authorize\">{hidden}\
                 <p><label>Email <input name=\"email\" type=\"email\" required></label></p>\
                 <p><label>Name <input name=\"name\"></label></p>\
                 <p><label><input name=\"email_verified\" type=\"checkbox\" value=\"true\" checked> Address verified</label></p>\
                 <p><button>Sign in</button></p></form></body></html>"
            ))
            .into_response()
        }
    }
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

async fn authorize_form(State(m): State<MockIdp>, Form(q): Form<BTreeMap<String, String>>) -> Response {
    if let Some(r) = refused(&m, &q) {
        return r;
    }
    let email = q.get("email").cloned().unwrap_or_default().trim().to_lowercase();
    if !email.contains('@') {
        return bad(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let digest = ring::digest::digest(&ring::digest::SHA256, email.as_bytes());
    let identity = Identity {
        sub: format!("1{}", digest.as_ref()[..8].iter().map(|b| format!("{b:03}")).collect::<String>()),
        name: q.get("name").cloned().filter(|n| !n.is_empty()).unwrap_or_else(|| email.clone()),
        email_verified: q.get("email_verified").is_some_and(|v| v == "true"),
        email,
    };
    grant(&m, &q, identity)
}

#[derive(Deserialize)]
struct TokenForm {
    grant_type: String,
    code: String,
    redirect_uri: String,
    code_verifier: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
}

/// The client's credentials: HTTP Basic (form-encoded parts) or in the body.
fn client_of(headers: &HeaderMap, f: &TokenForm) -> Option<(String, String)> {
    if let Some(basic) =
        headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Basic "))
    {
        let raw = String::from_utf8(STANDARD.decode(basic.trim()).ok()?).ok()?;
        let (id, secret) = raw.split_once(':')?;
        let decode =
            |s: &str| url::form_urlencoded::parse(format!("x={s}").as_bytes()).next().map(|(_, v)| v.into_owned());
        return Some((decode(id)?, decode(secret)?));
    }
    Some((f.client_id.clone()?, f.client_secret.clone()?))
}

async fn token(State(m): State<MockIdp>, headers: HeaderMap, Form(f): Form<TokenForm>) -> Response {
    let Some((client, secret)) = client_of(&headers, &f) else {
        return bad(StatusCode::UNAUTHORIZED, "invalid_client");
    };
    if m.inner.clients.get(&client) != Some(&secret) {
        return bad(StatusCode::UNAUTHORIZED, "invalid_client");
    }
    if f.grant_type != "authorization_code" {
        return bad(StatusCode::BAD_REQUEST, "unsupported_grant_type");
    }
    let Some(p) = m.inner.codes.lock().expect("lock").remove(&f.code) else {
        return bad(StatusCode::BAD_REQUEST, "invalid_grant");
    };
    let verifier_ok = match (&p.challenge, &f.code_verifier) {
        (Some(c), Some(v)) => B64.encode(ring::digest::digest(&ring::digest::SHA256, v.as_bytes()).as_ref()) == *c,
        _ => false,
    };
    if p.client_id != client
        || p.redirect_uri != f.redirect_uri
        || !verifier_ok
        || p.issued.elapsed() > Duration::from_secs(300)
    {
        return bad(StatusCode::BAD_REQUEST, "invalid_grant");
    }
    let iat = now();
    let mut claims = json!({
        "iss": m.inner.issuer,
        "aud": client,
        "azp": client,
        "sub": p.identity.sub,
        "email": p.identity.email,
        "email_verified": p.identity.email_verified,
        "name": p.identity.name,
        "iat": iat,
        "exp": iat + 3600,
    });
    if let Some(n) = p.nonce {
        claims["nonce"] = json!(n);
    }
    if let Some(serde_json::Value::Object(over)) = m.inner.tamper.lock().expect("lock").take() {
        for (k, v) in over {
            if v.is_null() {
                claims.as_object_mut().map(|c| c.remove(&k));
            } else {
                claims[k] = v;
            }
        }
    }
    Json(json!({
        "access_token": random(),
        "token_type": "Bearer",
        "expires_in": 3599,
        "scope": "openid email profile",
        "id_token": m.sign(&claims),
    }))
    .into_response()
}
