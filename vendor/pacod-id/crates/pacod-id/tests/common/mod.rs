//! A pacod-id started in process on a fresh database of the test PostgreSQL
//! (`scripts/test-services.sh`; PID_TEST_DATABASE_URL, by default 127.0.0.1:25434).

#![allow(dead_code)]

use std::net::TcpListener;

use pacod_id::{Bootstrap, Config, Listeners, Running};

pub fn server_url() -> String {
    std::env::var("PID_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://postgres:test@127.0.0.1:25434/pid".into())
}

/// Drops the databases of earlier runs (older than 30 minutes, by their UUIDv7 names), once
/// per test binary: every test makes one, and the test PostgreSQL's disk is not endless.
async fn sweep(conn: &mut sqlx::PgConnection) {
    static SWEPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if SWEPT.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let names: Vec<String> = sqlx::query_scalar("SELECT datname FROM pg_database WHERE datname LIKE 'pid_t_%'")
        .fetch_all(&mut *conn)
        .await
        .unwrap_or_default();
    for name in names {
        let made = name.strip_prefix("pid_t_").and_then(|h| u64::from_str_radix(h.get(..12)?, 16).ok());
        if made.is_some_and(|ms| now.saturating_sub(ms) > 30 * 60 * 1000) {
            let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)")).execute(&mut *conn).await;
        }
    }
}

/// A new, empty database; its URL.
pub async fn fresh_database() -> String {
    use sqlx::Connection;
    let server = server_url();
    let mut conn = sqlx::PgConnection::connect(&server).await.expect("the test PostgreSQL (make test-services)");
    sweep(&mut conn).await;
    let name = format!("pid_t_{}", uuid::Uuid::now_v7().simple());
    sqlx::query(&format!("CREATE DATABASE {name}")).execute(&mut conn).await.unwrap();
    let mut url = url::Url::parse(&server).unwrap();
    url.set_path(&format!("/{name}"));
    url.to_string()
}

pub const CLIENT_SECRET: &str = "a-console-secret-for-tests-0123456789";

/// The configuration of a test: two realms on the public listener's port (the second by the
/// name localhost), a confidential and a public client, a service client.
pub fn config_for(port: u16) -> String {
    format!(
        r#"
[[realm]]
name = "gateway"
issuer = "http://127.0.0.1:{port}"
brand = "gateway"
accounts_managed_by = ["gw.platform_admin"]

[[realm]]
name = "bazaar"
issuer = "http://localhost:{port}"
brand = "bazaar"
password_min_length = 8
self_signup = true

[[client]]
id = "console"
realm = "gateway"
kind = "confidential"
secret_env = "CONSOLE_SECRET"
redirect_uris = ["http://127.0.0.1:13000/auth/callback"]
post_logout_redirect_uris = ["http://127.0.0.1:13000/"]
audience = "pacod-gw"

[[client]]
id = "bazaar-android"
realm = "bazaar"
kind = "public"
redirect_uris = ["https://bazaar.example/native/oidc/callback"]
audience = "shop-api"

[[client]]
id = "gw-core"
realm = "gateway"
kind = "service"
product = "gw"
audience = "pacod-gw"
"#
    )
}

pub struct TestId {
    pub running: Running,
    /// Mail the service sent (an in-memory transport).
    pub sent: std::sync::Arc<std::sync::Mutex<Vec<pacod_id::mail::Sent>>>,
    pub db: String,
    pub public: String,
    pub internal: String,
    pub metrics: String,
    pub http: reqwest::Client,
}

impl TestId {
    pub async fn start() -> Self {
        Self::start_on(&fresh_database().await).await
    }

    /// On an existing database (a second instance of the same installation).
    pub async fn start_on(db: &str) -> Self {
        Self::start_with(db, |_| String::new(), |_| {}).await
    }

    /// With more configuration (TOML appended, given the public port) and a changed bootstrap.
    pub async fn start_with(
        db: &str,
        extra: impl FnOnce(u16) -> String,
        tweak: impl FnOnce(&mut pacod_id::Bootstrap),
    ) -> Self {
        Self::start_full(db, |port| config_for(port) + &extra(port), tweak).await
    }

    /// With a whole configuration of its own (given the public port).
    pub async fn start_full(
        db: &str,
        config: impl FnOnce(u16) -> String,
        tweak: impl FnOnce(&mut pacod_id::Bootstrap),
    ) -> Self {
        let bind = || {
            let l = TcpListener::bind("127.0.0.1:0").unwrap();
            l.set_nonblocking(true).unwrap();
            l
        };
        let listeners = Listeners { public: bind(), internal: bind(), metrics: bind() };
        let port = listeners.public.local_addr().unwrap().port();
        let config = Config::from_toml(&config(port)).unwrap();
        let mut boot = Bootstrap::for_tests(db);
        boot.client_secrets.insert("console".into(), CLIENT_SECRET.into());
        let sent = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        boot.mail = Some(pacod_id::mail::Transport::Memory(sent.clone()));
        tweak(&mut boot);
        let running = pacod_id::start(boot, config, listeners).await.expect("pacod-id starts");
        TestId {
            sent,
            public: format!("http://{}", running.public),
            internal: format!("http://{}", running.internal),
            metrics: format!("http://{}", running.metrics),
            db: db.to_owned(),
            running,
            http: reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap(),
        }
    }

    pub fn store(&self) -> &pacod_id::store::Store {
        &self.running.state.store
    }

    pub async fn get_json(&self, url: &str) -> (u16, serde_json::Value) {
        let r = self.http.get(url).send().await.unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(serde_json::Value::Null))
    }
}

// Signing in ----------------------------------------------------------------------------------

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use std::collections::BTreeMap;

pub const CONSOLE_REDIRECT: &str = "http://127.0.0.1:13000/auth/callback";
pub const PASSWORD: &str = "a long and unusual passphrase";

/// A browser: keeps cookies per name, sends an Origin on writes, follows nothing by itself.
pub struct Browser {
    pub http: reqwest::Client,
    pub cookies: BTreeMap<String, String>,
    pub origin: String,
    /// A Host header to send: the bazaar realm is served at localhost (on 127.0.0.1).
    pub host: Option<String>,
}

/// An answer: status, Location, Set-Cookie absorbed, body as JSON (or null).
#[derive(Debug)]
pub struct Answer {
    pub status: u16,
    pub location: Option<String>,
    pub json: serde_json::Value,
    pub text: String,
}

impl Browser {
    pub fn new(id: &TestId) -> Self {
        Browser { http: id.http.clone(), cookies: BTreeMap::new(), origin: id.public.clone(), host: None }
    }

    /// A browser on the bazaar realm (http://localhost:<port>).
    pub fn bazaar(id: &TestId) -> Self {
        let host = format!("localhost:{}", id.running.public.port());
        Browser { origin: format!("http://{host}"), host: Some(host), ..Browser::new(id) }
    }

    fn request(&self, method: reqwest::Method, url: &str) -> reqwest::RequestBuilder {
        let mut r = self.http.request(method, url).header("cookie", self.cookie_header());
        if let Some(h) = &self.host {
            r = r.header("host", h);
        }
        r
    }

    pub fn cookie_header(&self) -> String {
        self.cookies.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("; ")
    }

    async fn absorb(&mut self, r: reqwest::Response) -> Answer {
        for v in r.headers().get_all("set-cookie") {
            let v = v.to_str().unwrap();
            let (pair, attrs) = v.split_once(';').unwrap_or((v, ""));
            let (name, value) = pair.split_once('=').unwrap();
            if attrs.contains("Max-Age=0") || value.is_empty() {
                self.cookies.remove(name);
            } else {
                self.cookies.insert(name.to_owned(), value.to_owned());
            }
        }
        let status = r.status().as_u16();
        let location = r.headers().get("location").map(|l| l.to_str().unwrap().to_owned());
        let text = r.text().await.unwrap_or_default();
        Answer { status, location, json: serde_json::from_str(&text).unwrap_or(serde_json::Value::Null), text }
    }

    pub async fn get(&mut self, url: &str) -> Answer {
        let r = self.request(reqwest::Method::GET, url).send().await.unwrap();
        self.absorb(r).await
    }

    pub async fn patch(&mut self, url: &str, body: serde_json::Value) -> Answer {
        let r =
            self.request(reqwest::Method::PATCH, url).header("origin", &self.origin).json(&body).send().await.unwrap();
        self.absorb(r).await
    }

    pub async fn delete_json(&mut self, url: &str) -> Answer {
        let r = self.request(reqwest::Method::DELETE, url).header("origin", &self.origin).send().await.unwrap();
        self.absorb(r).await
    }

    pub async fn post(&mut self, url: &str, body: serde_json::Value) -> Answer {
        let r =
            self.request(reqwest::Method::POST, url).header("origin", &self.origin).json(&body).send().await.unwrap();
        self.absorb(r).await
    }
}

/// A PKCE pair: verifier and S256 challenge.
pub fn pkce() -> (String, String) {
    let verifier = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
    let digest = ring::digest::digest(&ring::digest::SHA256, verifier.as_bytes());
    (verifier.clone(), B64.encode(digest.as_ref()))
}

/// The authorize URL for the console, with `extra` parameters.
pub fn authorize_url(id: &TestId, challenge: &str, extra: &[(&str, &str)]) -> String {
    let mut u = url::Url::parse(&format!("{}/oidc/authorize", id.public)).unwrap();
    {
        let mut q = u.query_pairs_mut();
        for (k, v) in [
            ("response_type", "code"),
            ("client_id", "console"),
            ("redirect_uri", CONSOLE_REDIRECT),
            ("scope", "openid email profile"),
            ("state", "st-1"),
            ("nonce", "n-1"),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
        ] {
            if !extra.iter().any(|(ek, _)| *ek == k) {
                q.append_pair(k, v);
            }
        }
        for (k, v) in extra {
            if !v.is_empty() {
                q.append_pair(k, v);
            }
        }
    }
    u.into()
}

/// A query parameter of a URL.
pub fn param(url: &str, name: &str) -> Option<String> {
    url::Url::parse(url).unwrap().query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.into_owned())
}

/// An account with a password (`PASSWORD`), its address confirmed.
pub async fn account(id: &TestId, email: &str) -> pacod_id::store::Account {
    account_with(id, email, PASSWORD, true).await
}

pub async fn account_with(id: &TestId, email: &str, password: &str, verified: bool) -> pacod_id::store::Account {
    let hash = pacod_id::crypto::password::hash(password, Default::default()).unwrap();
    id.store()
        .account_create(&pacod_id::store::NewAccount {
            id: pacod_id::crypto::tokens::new_id("usr"),
            email: email.into(),
            display_name: "Ada Lovelace".into(),
            password_hash: Some(hash),
            verified_by: verified.then(|| "admin".to_owned()),
            signup_realm: "gateway".into(),
            ..Default::default()
        })
        .await
        .unwrap()
}

impl TestId {
    /// Signs `email` in through the console's flow in `b`; returns the code and the verifier.
    pub async fn sign_in(&self, b: &mut Browser, email: &str) -> (String, String) {
        let (verifier, challenge) = pkce();
        let a = b.get(&authorize_url(self, &challenge, &[])).await;
        assert_eq!(a.status, 303, "{a:?}");
        let to = a.location.unwrap();
        let flow = param(&to, "flow").unwrap_or_else(|| panic!("no flow: {to}"));
        let a = b
            .post(
                &format!("{}/ui-api/v1/flows/{flow}/password", self.public),
                serde_json::json!({ "email": email, "password": PASSWORD }),
            )
            .await;
        assert_eq!(a.status, 200, "{a:?}");
        assert_eq!(a.json["next"], "redirect", "{a:?}");
        let to = a.json["to"].as_str().unwrap();
        (param(to, "code").unwrap(), verifier)
    }

    /// Redeems a code at the token endpoint as the console.
    pub async fn redeem(&self, code: &str, verifier: &str) -> (u16, serde_json::Value) {
        self.token(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", CONSOLE_REDIRECT),
            ("code_verifier", verifier),
        ])
        .await
    }

    /// A token request as the console (HTTP Basic).
    pub async fn token(&self, form: &[(&str, &str)]) -> (u16, serde_json::Value) {
        let r = self
            .http
            .post(format!("{}/oidc/token", self.public))
            .basic_auth("console", Some(CLIENT_SECRET))
            .form(form)
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json().await.unwrap())
    }

    /// Checks a token against the published keys; returns header typ and claims.
    pub async fn verify(&self, token: &str) -> (String, serde_json::Value) {
        let set: pacod_id_jose::JwkSet =
            self.http.get(format!("{}/oidc/jwks", self.public)).send().await.unwrap().json().await.unwrap();
        let v = pacod_id_jose::Verifier::from_jwks(&set)
            .verify(token, &[pacod_id_jose::Alg::RS256, pacod_id_jose::Alg::ES256])
            .unwrap();
        (v.header.typ.unwrap_or_default(), v.claims)
    }

    /// Sends what is queued and returns what was sent since the last call.
    pub async fn mail(&self) -> Vec<pacod_id::mail::Sent> {
        pacod_id::mail::deliver_due(&self.running.state).await.unwrap();
        std::mem::take(&mut *self.sent.lock().unwrap())
    }

    /// A call to the internal listener with HTTP Basic, a JSON body or none.
    pub async fn internal_call(
        &self,
        method: reqwest::Method,
        path: &str,
        client: &str,
        secret: &str,
        body: Option<serde_json::Value>,
    ) -> (u16, serde_json::Value) {
        let mut r = self.http.request(method, format!("{}{path}", self.internal)).basic_auth(client, Some(secret));
        if let Some(b) = body {
            r = r.json(&b);
        }
        let r = r.send().await.unwrap();
        let status = r.status().as_u16();
        (status, r.json().await.unwrap_or(serde_json::Value::Null))
    }

    /// Lets a service client in with `secret`.
    pub async fn service_secret(&self, client: &str, secret: &str) {
        let realm = self.running.state.config.clients[client].realm.clone();
        self.store()
            .client_set_secret(client, &realm, &self.running.state.boot.token_pepper.hash(secret))
            .await
            .unwrap();
    }

    pub async fn sql(&self, q: &str) {
        sqlx::query(q).execute(self.store().pool()).await.unwrap();
    }
}
