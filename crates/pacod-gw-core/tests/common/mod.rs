//! Shared harness for the integration tests: a database per test, a gateway started in
//! process, a sink that plays the analytics ingest endpoint, and a small admin client.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use pacod_gw_core::app::App;
use pacod_gw_core::bootstrap::Bootstrap;
use pacod_gw_events::UsageEvent;
use parking_lot::Mutex;
use serde_json::{Value, json};
use sqlx::Connection;

pub const ADMIN_EMAIL: &str = "admin@example.test";
pub const ADMIN_PASSWORD: &str = "correct horse battery";

/// The PostgreSQL server the tests may create databases on.
fn admin_url() -> String {
    std::env::var("GW_TEST_DATABASE_URL").unwrap_or_else(|_| "postgres://postgres:test@localhost:25433/gw".into())
}

/// Drops the databases of earlier runs (older than 30 minutes, by their UUIDv7 names), once per
/// test binary: every test makes one, and the test PostgreSQL's disk is not endless.
async fn sweep(conn: &mut sqlx::PgConnection) {
    static SWEPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if SWEPT.swap(true, std::sync::atomic::Ordering::SeqCst) {
        return;
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as u64;
    let names: Vec<String> = sqlx::query_scalar("SELECT datname FROM pg_database WHERE datname LIKE 'gw\\_t\\_%'")
        .fetch_all(&mut *conn)
        .await
        .unwrap_or_default();
    for name in names {
        let made = name.strip_prefix("gw_t_").and_then(|h| u64::from_str_radix(h.get(..12)?, 16).ok());
        if made.is_some_and(|ms| now.saturating_sub(ms) > 30 * 60 * 1000) {
            let _ = sqlx::query(&format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)")).execute(&mut *conn).await;
        }
    }
}

/// Creates an empty database for one test and returns its URL. The in-process pacod-id of the
/// OIDC tests takes its database from here too, so the sweep covers both.
pub async fn fresh_database() -> String {
    let admin = admin_url();
    let name = format!("gw_t_{}", uuid::Uuid::now_v7().simple());
    let mut conn = sqlx::PgConnection::connect(&admin).await.unwrap_or_else(|e| {
        panic!(
            "the integration tests need PostgreSQL at {admin} (set GW_TEST_DATABASE_URL, or run `make test-services`): {e}"
        )
    });
    sweep(&mut conn).await;
    sqlx::query(&format!("CREATE DATABASE {name}")).execute(&mut conn).await.expect("create database");
    let (base, _) = admin.rsplit_once('/').expect("database URL has a path");
    format!("{base}/{name}")
}

pub fn temp_dir(label: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("pgw-it-{label}-{}", uuid::Uuid::now_v7().simple()));
    std::fs::create_dir_all(&d).expect("temp dir");
    d
}

/// One batch as the ingest sink received it.
#[derive(Debug, Clone)]
pub struct Batch {
    pub path: String,
    pub batch_id: String,
    pub token: String,
    pub lines: Vec<String>,
}

#[derive(Default)]
struct SinkState {
    batches: Mutex<Vec<Batch>>,
    failing: AtomicBool,
}

/// Plays the analytics service's ingest endpoints and records what it is sent.
pub struct IngestSink {
    pub url: String,
    state: Arc<SinkState>,
}

impl IngestSink {
    pub async fn start() -> Self {
        let state = Arc::new(SinkState::default());
        async fn ingest(
            State((state, path)): State<(Arc<SinkState>, &'static str)>,
            headers: HeaderMap,
            body: Bytes,
        ) -> (StatusCode, axum::Json<Value>) {
            if state.failing.load(Ordering::SeqCst) {
                return (StatusCode::SERVICE_UNAVAILABLE, axum::Json(json!({ "error": "down" })));
            }
            let h = |n: &str| headers.get(n).and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
            let lines: Vec<String> =
                String::from_utf8_lossy(&body).lines().filter(|l| !l.is_empty()).map(str::to_owned).collect();
            let rows = lines.len();
            let batch_id = h("x-gw-batch-id");
            state.batches.lock().push(Batch {
                path: path.to_owned(),
                batch_id: batch_id.clone(),
                token: h("x-gw-internal-token"),
                lines,
            });
            (StatusCode::OK, axum::Json(json!({ "batch_id": batch_id, "rows": rows })))
        }
        let app = Router::new()
            .route("/ingest/usage", post(ingest).with_state((state.clone(), "/ingest/usage")))
            .route("/ingest/audit", post(ingest).with_state((state.clone(), "/ingest/audit")))
            .route("/readyz", axum::routing::get(|| async { "ok" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let addr = listener.local_addr().expect("addr");
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Self { url: format!("http://{addr}"), state }
    }

    pub fn set_failing(&self, failing: bool) {
        self.state.failing.store(failing, Ordering::SeqCst);
    }

    pub fn batches(&self) -> Vec<Batch> {
        self.state.batches.lock().clone()
    }

    /// The distinct usage events received so far, in arrival order.
    pub fn events(&self) -> Vec<UsageEvent> {
        let mut seen = std::collections::HashSet::new();
        self.batches()
            .iter()
            .filter(|b| b.path == "/ingest/usage")
            .flat_map(|b| b.lines.iter())
            .map(|l| serde_json::from_str::<UsageEvent>(l).unwrap_or_else(|e| panic!("bad event line: {e}\n{l}")))
            .filter(|e| seen.insert(e.id))
            .collect()
    }

    /// The distinct audit records received so far, in arrival order.
    pub fn audit_records(&self) -> Vec<pacod_gw_events::audit::AuditRecord> {
        let mut seen = std::collections::HashSet::new();
        self.batches()
            .iter()
            .filter(|b| b.path == "/ingest/audit")
            .flat_map(|b| b.lines.iter())
            .map(|l| {
                serde_json::from_str::<pacod_gw_events::audit::AuditRecord>(l)
                    .unwrap_or_else(|e| panic!("bad audit line: {e}\n{l}"))
            })
            .filter(|r| seen.insert(r.audit_id.clone()))
            .collect()
    }

    /// Waits until at least `n` audit records have arrived.
    pub async fn wait_audit(&self, n: usize) -> Vec<pacod_gw_events::audit::AuditRecord> {
        let start = Instant::now();
        loop {
            let records = self.audit_records();
            if records.len() >= n {
                return records;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "expected {n} audit records, got {} after 10 s",
                records.len()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Waits until at least `n` distinct events have arrived.
    pub async fn wait_events(&self, n: usize) -> Vec<UsageEvent> {
        let start = Instant::now();
        loop {
            let ev = self.events();
            if ev.len() >= n {
                return ev;
            }
            assert!(
                start.elapsed() < Duration::from_secs(10),
                "expected {n} usage events, got {} after 10 s",
                ev.len()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// A gateway running in the test process.
pub struct TestGw {
    pub app: App,
    pub data: String,
    pub admin: String,
    pub http: reqwest::Client,
    pub token: String,
    pub database_url: String,
}

pub struct Created {
    pub id: String,
    pub body: Value,
}

impl TestGw {
    /// Starts a gateway on a fresh database, shipping events to `sink` when one is given.
    pub async fn start(sink: Option<&IngestSink>) -> Self {
        let db = fresh_database().await;
        Self::start_on(&db, sink, |_| {}).await
    }

    pub async fn start_on(database_url: &str, sink: Option<&IngestSink>, tweak: impl FnOnce(&mut Bootstrap)) -> Self {
        let mut boot = Bootstrap::for_tests(database_url, temp_dir("spool"));
        boot.analytics_url = sink.map(|s| s.url.clone());
        tweak(&mut boot);
        let app = App::start(boot, None).await.expect("gateway starts");
        let http = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().expect("client");
        let admin = format!("http://{}", app.admin_addr.expect("admin listener"));
        // Empty for the admin role, which serves no data plane.
        let data = app.data_addr.map(|a| format!("http://{a}")).unwrap_or_default();
        let mut gw = Self { app, data, admin, http, token: String::new(), database_url: database_url.to_owned() };
        gw.token = gw.login(ADMIN_EMAIL, ADMIN_PASSWORD).await;
        gw
    }

    pub fn data_addr(&self) -> SocketAddr {
        self.app.data_addr.expect("data listener")
    }

    /// Waits until every running instance has applied the newest configuration version, as
    /// the console's "applied on n of n" shows it.
    pub async fn wait_applied(&self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let (st, v) = self.api("GET", "/system/status", None).await;
            assert_eq!(st, 200, "{v}");
            let total = v["instances"].as_array().map_or(0, Vec::len);
            if total > 0 && v["instances_current"].as_u64() == Some(total as u64) {
                return;
            }
            assert!(Instant::now() < deadline, "not every instance applied the configuration: {v}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    pub async fn login(&self, email: &str, password: &str) -> String {
        let r = self
            .http
            .post(format!("{}/api/v1/session", self.admin))
            .json(&json!({ "email": email, "password": password }))
            .send()
            .await
            .expect("login request");
        assert_eq!(r.status(), 200, "login failed: {}", r.text().await.unwrap_or_default());
        r.json::<Value>().await.expect("json")["token"].as_str().expect("token").to_owned()
    }

    /// Calls the management API with the admin token and returns status and JSON body.
    pub async fn api(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.api_as(&self.token, method, path, body).await
    }

    /// As [`api`](Self::api). An update without a `version` gets the version the object has
    /// now, as a client that read it first would send: the API requires one.
    pub async fn api_as(&self, token: &str, method: &str, path: &str, mut body: Option<Value>) -> (u16, Value) {
        if method == "PUT"
            && let Some(Value::Object(b)) = body.as_mut()
            && !b.contains_key("version")
        {
            let (st, current) = Box::pin(self.api_as(token, "GET", path, None)).await;
            if st == 200
                && let Some(v) = current.get("version").filter(|v| v.is_i64())
            {
                b.insert("version".into(), v.clone());
            }
        }
        let mut req = self
            .http
            .request(method.parse().expect("method"), format!("{}/api/v1{path}", self.admin))
            .bearer_auth(token);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let r = req.send().await.expect("request");
        let status = r.status().as_u16();
        let text = r.text().await.expect("body");
        let value =
            if text.is_empty() { Value::Null } else { serde_json::from_str(&text).unwrap_or(Value::String(text)) };
        (status, value)
    }

    /// Creates an object and returns its id, asserting success.
    pub async fn create(&self, path: &str, body: Value) -> Created {
        let (status, v) = self.api("POST", path, Some(body)).await;
        assert_eq!(status, 201, "POST {path} failed: {v}");
        Created { id: v["id"].as_str().expect("id").to_owned(), body: v }
    }

    /// Sets up the usual fixture: an organisation, a workspace, a provider pointing at
    /// `base_url` with a shared credential, and an API key. Returns the fixture.
    pub async fn fixture(&self, family: &str, base_url: &str) -> Fixture {
        let org = self.create("/orgs", json!({ "name": "acme" })).await.id;
        let ws = self
            .create(&format!("/orgs/{org}/workspaces"), json!({ "name": "prod", "tag_keys": ["purpose", "site"] }))
            .await
            .id;
        let provider =
            self.create("/providers", json!({ "name": family, "family": family, "base_url": base_url })).await.id;
        self.create(
            &format!("/providers/{provider}/credentials"),
            json!({ "label": "shared", "secret": PROVIDER_SECRET }),
        )
        .await;
        let key = self.create(&format!("/orgs/{org}/workspaces/{ws}/keys"), json!({ "name": "svc" })).await;
        let fx = Fixture {
            org,
            ws,
            provider,
            key_id: key.id,
            key: key.body["secret"].as_str().expect("secret").to_owned(),
            provider_name: family.to_owned(),
        };
        // The models the tests name by the provider's id. Proxy mode serves only models the
        // repository knows and the workspace was granted.
        for upstream in ["m", "gpt-test", "claude-test"] {
            self.model(&fx, &format!("{family}/{upstream}"), upstream, json!({ "free": true })).await;
        }
        fx
    }

    /// Registers a model at the fixture's provider, activates it and grants it to the
    /// fixture's organisation. `extra` adds to or overrides the model's fields; a model
    /// without `free` needs a price before this is called, so pass `"free": true` or use
    /// [`priced_model`](Self::priced_model).
    pub async fn model(&self, fx: &Fixture, name: &str, upstream_id: &str, extra: Value) -> String {
        let id = self.draft_model(&fx.provider, name, upstream_id, extra).await;
        self.activate(&id).await;
        self.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
        id
    }

    /// As [`model`](Self::model), with a price per million tokens instead of "free".
    pub async fn priced_model(
        &self,
        fx: &Fixture,
        name: &str,
        upstream_id: &str,
        extra: Value,
        rates: Value,
    ) -> String {
        let id = self.draft_model(&fx.provider, name, upstream_id, extra).await;
        let (st, v) = self.api("PUT", &format!("/models/{id}/price"), Some(rates)).await;
        assert_eq!(st, 201, "price: {v}");
        self.activate(&id).await;
        self.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
        id
    }

    pub async fn draft_model(&self, provider: &str, name: &str, upstream_id: &str, extra: Value) -> String {
        let mut body = json!({ "name": name, "provider_id": provider, "upstream_id": upstream_id });
        if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
            b.extend(e.clone());
        }
        self.create("/models", body).await.id
    }

    pub async fn activate(&self, model_id: &str) {
        let (st, v) = self
            .api("POST", &format!("/models/{model_id}/transition"), Some(json!({ "to": "active", "skip_probe": true })))
            .await;
        assert_eq!(st, 200, "activation: {v}");
    }

    /// Creates a template in the fixture's workspace from a spec.
    pub async fn template(&self, fx: &Fixture, name: &str, spec: Value) -> Created {
        self.create(&format!("/orgs/{}/workspaces/{}/templates", fx.org, fx.ws), json!({ "name": name, "spec": spec }))
            .await
    }

    /// Creates another API key in the fixture's workspace and returns its secret.
    pub async fn key(&self, fx: &Fixture, settings: Value) -> String {
        let k = self.create(&format!("/orgs/{}/workspaces/{}/keys", fx.org, fx.ws), settings).await;
        k.body["secret"].as_str().expect("secret").to_owned()
    }
}

/// A gateway-only instance on the same database, as another machine would run it.
pub struct TestNode {
    pub app: App,
    pub data: String,
    pub spool_dir: PathBuf,
}

impl TestNode {
    pub async fn start(database_url: &str, tweak: impl FnOnce(&mut Bootstrap)) -> Self {
        let spool_dir = temp_dir("node");
        let mut boot = Bootstrap::for_tests(database_url, &spool_dir);
        boot.role = pacod_gw_core::bootstrap::Role::Gateway;
        tweak(&mut boot);
        let app = App::start(boot, None).await.expect("node starts");
        let data = format!("http://{}", app.data_addr.expect("data listener"));
        Self { app, data, spool_dir }
    }

    pub fn instance_id(&self) -> &str {
        &self.app.gw.boot.instance_id
    }
}

/// The provider key the fixture stores. It must never appear in anything the gateway returns.
pub const PROVIDER_SECRET: &str = "sk-test-provider-secret-ZZ99";

pub struct Fixture {
    pub org: String,
    pub ws: String,
    pub provider: String,
    pub provider_name: String,
    pub key_id: String,
    /// The full gateway API key.
    pub key: String,
}

/// A mail server that accepts everything and keeps the messages, for the account emails.
pub struct MailSink {
    pub port: u16,
    mails: Arc<Mutex<Vec<String>>>,
}

impl MailSink {
    pub async fn start() -> Self {
        use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let mails = Arc::new(Mutex::new(Vec::new()));
        let store = mails.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let store = store.clone();
                tokio::spawn(async move {
                    let (r, mut w) = stream.into_split();
                    let mut lines = BufReader::new(r).lines();
                    let _ = w.write_all(b"220 sink ESMTP\r\n").await;
                    while let Ok(Some(line)) = lines.next_line().await {
                        let cmd = line.trim_end_matches('\r').to_ascii_uppercase();
                        let reply: &[u8] = if cmd.starts_with("DATA") {
                            let _ = w.write_all(b"354 go ahead\r\n").await;
                            let mut body = Vec::new();
                            while let Ok(Some(l)) = lines.next_line().await {
                                let l = l.trim_end_matches('\r');
                                if l == "." {
                                    break;
                                }
                                body.push(l.strip_prefix('.').filter(|_| l.starts_with("..")).unwrap_or(l).to_owned());
                            }
                            store.lock().push(body.join("\n"));
                            b"250 queued\r\n"
                        } else if cmd.starts_with("QUIT") {
                            let _ = w.write_all(b"221 bye\r\n").await;
                            break;
                        } else {
                            b"250 ok\r\n"
                        };
                        let _ = w.write_all(reply).await;
                    }
                });
            }
        });
        Self { port, mails }
    }

    /// Settings that send to this sink, with links to `http://console.test`.
    pub fn config(&self) -> pacod_gw_core::bootstrap::EmailConfig {
        pacod_gw_core::bootstrap::EmailConfig {
            smtp_host: "127.0.0.1".into(),
            smtp_port: self.port,
            smtp_username: None,
            smtp_password: None,
            smtp_tls: pacod_gw_core::bootstrap::SmtpTls::None,
            smtp_timeout_seconds: 5,
            from_address: "gateway@example.test".into(),
            from_name: "pacod-gw".into(),
            console_url: "http://console.test".into(),
        }
    }

    /// The messages received so far, with quoted-printable soft breaks and escapes undone.
    pub fn mails(&self) -> Vec<String> {
        self.mails.lock().iter().map(|m| decode_qp(m)).collect()
    }

    /// Waits until `n` messages have arrived.
    pub async fn wait(&self, n: usize) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let mails = self.mails();
            if mails.len() >= n {
                return mails;
            }
            assert!(Instant::now() < deadline, "expected {n} emails, got {}", mails.len());
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// The token of the console link `path` in a message.
pub fn link_token(mail: &str, path: &str) -> String {
    let start =
        mail.find(&format!("http://console.test{path}?token=")).unwrap_or_else(|| panic!("no {path} link in {mail}"))
            + format!("http://console.test{path}?token=").len();
    mail[start..].chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_').collect()
}

fn decode_qp(raw: &str) -> String {
    let joined = raw.replace("=\n", "");
    let bytes = joined.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'='
            && i + 2 < bytes.len()
            && bytes[i + 1].is_ascii_hexdigit()
            && bytes[i + 2].is_ascii_hexdigit()
        {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).expect("ascii");
            out.push(u8::from_str_radix(hex, 16).expect("hex"));
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
