//! Test support: an in-process service on an ephemeral port, backed by its own database in a
//! real ClickHouse, and the fixed data set the report tests assert against.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use pacod_gw_analytics::storage::{Binds, ClickHouse};
use pacod_gw_analytics::{AppState, Config};
use pacod_gw_events::ingest::{AUDIT_PATH, BATCH_ID_HEADER, INTERNAL_TOKEN_HEADER, NDJSON, USAGE_PATH};
use pacod_gw_events::{AuditPart, AuditRecord, UsageEvent};
use reqwest::StatusCode;
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use uuid::Uuid;

pub const TOKEN: &str = "integration-test-token-0123456789";

/// The ClickHouse the tests run against. There is no fallback: without it the tests fail.
pub fn clickhouse_url() -> String {
    match std::env::var("GW_TEST_CLICKHOUSE_URL") {
        Ok(url) if !url.trim().is_empty() => url,
        _ => panic!(
            "GW_TEST_CLICKHOUSE_URL is not set. These tests need a real ClickHouse, for example:\n  \
             docker run -d --name pgw-test-ch -p 28123:8123 -e CLICKHOUSE_SKIP_USER_SETUP=1 \
             clickhouse/clickhouse-server:26.8.17.4\n  \
             export GW_TEST_CLICKHOUSE_URL=http://localhost:28123"
        ),
    }
}

pub fn config(clickhouse_url: &str, database: &str) -> Config {
    Config {
        listen: "127.0.0.1:0".parse().expect("address"),
        clickhouse_url: clickhouse_url.to_owned(),
        clickhouse_database: database.to_owned(),
        clickhouse_user: None,
        clickhouse_password: None,
        internal_token: TOKEN.to_owned(),
        max_batch_bytes: 8 * 1024 * 1024,
    }
}

/// A running service with a database of its own. The database is dropped with the value.
pub struct TestApp {
    pub base: String,
    pub http: reqwest::Client,
    /// Direct access to the service's database, for assertions on what is stored.
    pub clickhouse: ClickHouse,
    pub state: AppState,
    /// The ClickHouse this service was started against, and its database there.
    clickhouse_url: String,
    database: String,
    shutdown: Option<oneshot::Sender<()>>,
    pub server: Option<JoinHandle<anyhow::Result<()>>>,
}

impl TestApp {
    /// Starts the service with the migrations applied.
    pub async fn start() -> Self {
        let app = Self::start_without_migrations().await;
        app.state.migrate().await.expect("migrations apply");
        app
    }

    /// Starts the service the way it is before ClickHouse has become reachable.
    pub async fn start_without_migrations() -> Self {
        Self::start_against(&clickhouse_url()).await
    }

    pub async fn start_against(clickhouse_url: &str) -> Self {
        let database = format!("pgw_test_{}", Uuid::new_v4().simple());
        let config = config(clickhouse_url, &database);
        let state = AppState::new(&config).expect("valid configuration");
        let clickhouse = ClickHouse::new(clickhouse_url, &database, None, None).expect("client");

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind an ephemeral port");
        let base = format!("http://{}", listener.local_addr().expect("local address"));
        let (shutdown, stopped) = oneshot::channel::<()>();
        let server = tokio::spawn(pacod_gw_analytics::serve(listener, state.clone(), async {
            let _ = stopped.await;
        }));

        Self {
            base,
            http: reqwest::Client::new(),
            clickhouse,
            state,
            clickhouse_url: clickhouse_url.to_owned(),
            database,
            shutdown: Some(shutdown),
            server: Some(server),
        }
    }

    /// Asks the service to stop, as SIGTERM does.
    pub fn stop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
    }

    /// A request that carries the internal token.
    pub fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http.request(method, format!("{}{path}", self.base)).header(INTERNAL_TOKEN_HEADER, TOKEN)
    }

    pub async fn get(&self, path: &str) -> reqwest::Response {
        self.request(reqwest::Method::GET, path).send().await.expect("the service answers")
    }

    /// `GET path`, which must answer 200 with JSON.
    pub async fn json(&self, path: &str) -> Value {
        let response = self.get(path).await;
        let status = response.status();
        let text = response.text().await.expect("body");
        assert_eq!(status, StatusCode::OK, "GET {path}: {text}");
        serde_json::from_str(&text).expect("JSON body")
    }

    /// `GET path`, which must fail with `status`; returns the error object.
    pub async fn error(&self, path: &str, status: StatusCode) -> Value {
        let response = self.get(path).await;
        let got = response.status();
        let text = response.text().await.expect("body");
        assert_eq!(got, status, "GET {path}: {text}");
        let body: Value = serde_json::from_str(&text).expect("JSON error body");
        body["error"].clone()
    }

    pub async fn post_batch(&self, path: &str, batch_id: &str, body: String) -> reqwest::Response {
        self.request(reqwest::Method::POST, path)
            .header(BATCH_ID_HEADER, batch_id)
            .header("content-type", NDJSON)
            .body(body)
            .send()
            .await
            .expect("the service answers")
    }

    /// Ships usage events as one batch, which must be accepted.
    pub async fn store_usage(&self, batch_id: &str, events: &[UsageEvent]) {
        let response = self.post_batch(USAGE_PATH, batch_id, ndjson(events)).await;
        assert_accepted(response, batch_id, events.len()).await;
    }

    /// Ships audit records as one batch, which must be accepted.
    pub async fn store_audit(&self, batch_id: &str, records: &[AuditRecord]) {
        let response = self.post_batch(AUDIT_PATH, batch_id, ndjson(records)).await;
        assert_accepted(response, batch_id, records.len()).await;
    }

    /// Runs a query directly against the service's database.
    pub async fn select(&self, sql: &str) -> Vec<Value> {
        self.clickhouse.fetch(sql, &Binds::new()).await.expect("direct ClickHouse query")
    }

    /// Runs a statement directly against the service's database.
    pub async fn command(&self, sql: &str) {
        self.clickhouse.command(sql, &Binds::new(), &[]).await.expect("direct ClickHouse statement");
    }

    /// The number of rows physically stored in `table`: duplicates are counted.
    pub async fn stored_rows(&self, table: &str) -> u64 {
        let rows = self.select(&format!("SELECT count() AS rows FROM {table}")).await;
        rows[0]["rows"].as_u64().expect("count")
    }

    /// The numbers of the fixture events (see [`fixture`]) a request list returns, in order.
    pub async fn request_numbers(&self, query: &str) -> Vec<u128> {
        let page = self.json(&format!("/reports/requests?{query}")).await;
        numbers(&page["requests"])
    }
}

impl Drop for TestApp {
    fn drop(&mut self) {
        self.stop();
        // Dropping the database needs a request, and `drop` cannot await: use a thread with a
        // runtime of its own. Best effort; a test that panicked still cleans up.
        let endpoint = self.clickhouse_url.clone();
        let statement = format!("DROP DATABASE IF EXISTS `{}` SYNC", self.database);
        let cleanup = std::thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
                return;
            };
            runtime.block_on(async {
                let _ = reqwest::Client::new().post(endpoint).body(statement).send().await;
            });
        });
        let _ = cleanup.join();
    }
}

async fn assert_accepted(response: reqwest::Response, batch_id: &str, rows: usize) {
    let status = response.status();
    let text = response.text().await.expect("body");
    assert_eq!(status, StatusCode::OK, "ingest of {batch_id}: {text}");
    let reply: Value = serde_json::from_str(&text).expect("JSON reply");
    assert_eq!(reply, json!({ "batch_id": batch_id, "rows": rows }));
}

pub fn ndjson<T: Serialize>(records: &[T]) -> String {
    records.iter().map(|record| serde_json::to_string(record).expect("serialisable") + "\n").collect()
}

pub fn at(time: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(time).expect("valid time").with_timezone(&Utc)
}

/// The id of fixture event `number`.
pub fn id(number: u128) -> Uuid {
    Uuid::from_u128(number)
}

/// The fixture numbers of a JSON list of events, in order.
pub fn numbers(events: &Value) -> Vec<u128> {
    events
        .as_array()
        .expect("a list of events")
        .iter()
        .map(|event| {
            let id: Uuid = event["id"].as_str().expect("id").parse().expect("a UUID");
            id.as_u128()
        })
        .collect()
}

/// The range that covers the March events of the fixture.
pub const MARCH: &str = "from=2026-03-01T00:00:00Z&to=2026-03-13T00:00:00Z";

/// What one fixture event differs in from the others.
pub struct Call {
    pub number: u128,
    pub time: &'static str,
    pub organisation: &'static str,
    pub workspace: &'static str,
    pub provider: &'static str,
    pub model: &'static str,
    pub template: &'static str,
    pub key: &'static str,
    pub end_user: &'static str,
    pub status: &'static str,
    pub error_type: &'static str,
    pub mode: &'static str,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    pub reasoning: u64,
    pub cost: Option<f64>,
    pub delay: f32,
    pub purpose: &'static str,
    pub usage_source: &'static str,
    pub price_missing: bool,
    pub trace: &'static str,
    pub session: &'static str,
    pub audit: &'static str,
    pub instance: &'static str,
}

impl Call {
    pub fn event(&self) -> UsageEvent {
        let mut event = UsageEvent::new(id(self.number), self.instance);
        event.timestamp = at(self.time);
        event.organisation_id = self.organisation.into();
        event.workspace_id = self.workspace.into();
        event.key_id = self.key.into();
        event.end_user = self.end_user.into();
        event.session_id = self.session.into();
        event.trace_id = self.trace.into();
        if !self.purpose.is_empty() {
            event.tags = BTreeMap::from([("purpose".to_owned(), self.purpose.to_owned())]);
        }
        event.mode = self.mode.into();
        event.interface = if self.mode == "proxy" { "proxy" } else { "openai" }.into();
        event.provider = self.provider.into();
        event.model_requested = self.model.into();
        event.model_served = self.model.into();
        event.template = self.template.into();
        event.status = self.status.into();
        event.http_status = if self.status == "ok" { 200 } else { 500 };
        event.error_type = self.error_type.into();
        event.attempts = 1;
        event.timing.gateway_delay_ms = self.delay;
        event.timing.pre_ms = self.delay;
        event.timing.receive_ms = 1.0;
        event.timing.total_ms = 250.0;
        event.usage.input_tokens = self.input;
        event.usage.output_tokens = self.output;
        event.usage.cache_read_tokens = self.cache_read;
        event.usage.cache_write_tokens = self.cache_write;
        event.usage.reasoning_tokens = self.reasoning;
        event.usage_source = self.usage_source.into();
        event.cost = self.cost;
        event.currency = "USD".into();
        event.price_missing = self.price_missing;
        event.audit_id = self.audit.into();
        event
    }
}

const BASE: Call = Call {
    number: 0,
    time: "",
    organisation: "org-a",
    workspace: "ws-1",
    provider: "",
    model: "",
    template: "",
    key: "key-1",
    end_user: "",
    status: "ok",
    error_type: "",
    mode: "managed",
    input: 0,
    output: 0,
    cache_read: 0,
    cache_write: 0,
    reasoning: 0,
    cost: None,
    delay: 0.0,
    purpose: "",
    usage_source: "provider",
    price_missing: false,
    trace: "",
    session: "",
    audit: "",
    instance: "i-1",
};

/// The fixed data set. Costs are binary fractions, so their sums are exact.
///
/// ```text
///  #  time (2026)   org/ws       provider/model           user   status    mode     in    out  cost  delay
///  1  03-12 10:05   org-a/ws-1   anthropic/claude-haiku   alice  ok        managed  100   50   0.5   2.0
///  2  03-12 10:20   org-a/ws-1   anthropic/claude-haiku   alice  error     managed  20    0    -     4.0
///  3  03-12 08:00   org-a/ws-1   openai/gpt-mini          bob    ok        proxy    200   100  1.0   1.0
///  4  03-10 15:00   org-a/ws-1   openai/gpt-mini          bob    timeout   proxy    300   0    0.25  8.0
///  5  03-03 12:00   org-a/ws-1   anthropic/claude-sonnet  alice  ok        managed  1000  500  2.0   3.0
///  6  03-12 09:00   org-a/ws-2   openai/gpt-mini          carol  ok        proxy    400   200  -     5.0
///  7  03-12 10:10   org-b/ws-9   anthropic/claude-haiku   zed    ok        proxy    7     3    4.0   100.0
///  8  02-20 12:00   org-a/ws-1   anthropic/claude-haiku   alice  ok        managed  10000 5000 8.0   6.0
///  9  03-12 10:25   org-a/ws-1   (none)                   -      rejected  managed  0     0    -     0.5
/// ```
///
/// 2026-03-12 is a Thursday; its week starts on Monday 2026-03-09.
pub fn fixture() -> Vec<UsageEvent> {
    [
        Call {
            number: 1,
            time: "2026-03-12T10:05:00Z",
            provider: "anthropic",
            model: "claude-haiku",
            template: "planner",
            end_user: "alice",
            input: 100,
            output: 50,
            cache_read: 10,
            cache_write: 5,
            cost: Some(0.5),
            delay: 2.0,
            purpose: "plan",
            trace: "trace-1",
            session: "sess-1",
            audit: "aud-1",
            ..BASE
        },
        Call {
            number: 2,
            time: "2026-03-12T10:20:00Z",
            provider: "anthropic",
            model: "claude-haiku",
            template: "planner",
            end_user: "alice",
            status: "error",
            error_type: "rate_limited",
            input: 20,
            delay: 4.0,
            purpose: "plan",
            usage_source: "missing",
            trace: "trace-1",
            session: "sess-1",
            ..BASE
        },
        Call {
            number: 3,
            time: "2026-03-12T08:00:00Z",
            provider: "openai",
            model: "gpt-mini",
            key: "key-2",
            end_user: "bob",
            mode: "proxy",
            input: 200,
            output: 100,
            reasoning: 40,
            cost: Some(1.0),
            delay: 1.0,
            purpose: "chat",
            trace: "trace-2",
            instance: "i-2",
            ..BASE
        },
        Call {
            number: 4,
            time: "2026-03-10T15:00:00Z",
            provider: "openai",
            model: "gpt-mini",
            key: "key-2",
            end_user: "bob",
            status: "timeout",
            error_type: "upstream_timeout",
            mode: "proxy",
            input: 300,
            cost: Some(0.25),
            delay: 8.0,
            usage_source: "estimated",
            instance: "i-2",
            ..BASE
        },
        Call {
            number: 5,
            time: "2026-03-03T12:00:00Z",
            provider: "anthropic",
            model: "claude-sonnet",
            template: "writer",
            end_user: "alice",
            input: 1000,
            output: 500,
            cache_read: 100,
            cache_write: 50,
            reasoning: 200,
            cost: Some(2.0),
            delay: 3.0,
            purpose: "write",
            ..BASE
        },
        Call {
            number: 6,
            time: "2026-03-12T09:00:00Z",
            workspace: "ws-2",
            provider: "openai",
            model: "gpt-mini",
            key: "key-3",
            end_user: "carol",
            mode: "proxy",
            input: 400,
            output: 200,
            delay: 5.0,
            price_missing: true,
            instance: "i-2",
            ..BASE
        },
        Call {
            number: 7,
            time: "2026-03-12T10:10:00Z",
            organisation: "org-b",
            workspace: "ws-9",
            provider: "anthropic",
            model: "claude-haiku",
            key: "key-9",
            end_user: "zed",
            mode: "proxy",
            input: 7,
            output: 3,
            cost: Some(4.0),
            delay: 100.0,
            trace: "trace-9",
            instance: "i-3",
            ..BASE
        },
        Call {
            number: 8,
            time: "2026-02-20T12:00:00Z",
            provider: "anthropic",
            model: "claude-haiku",
            template: "planner",
            end_user: "alice",
            input: 10_000,
            output: 5_000,
            cost: Some(8.0),
            delay: 6.0,
            ..BASE
        },
        Call {
            number: 9,
            time: "2026-03-12T10:25:00Z",
            status: "rejected",
            error_type: "quota_exceeded",
            delay: 0.5,
            usage_source: "missing",
            ..BASE
        },
    ]
    .iter()
    .map(Call::event)
    .collect()
}

/// A started service that holds the fixture.
pub async fn app_with_fixture() -> TestApp {
    let app = TestApp::start().await;
    app.store_usage("fixture-1", &fixture()).await;
    app
}

/// The standard counters of an aggregated row, in the order the API returns them:
/// requests, errors, input, output, cache read, cache write, reasoning, cost.
pub fn totals(counts: [u64; 7], cost: f64) -> Value {
    let [requests, errors, input, output, cache_read, cache_write, reasoning] = counts;
    json!({
        "requests": requests,
        "errors": errors,
        "input_tokens": input,
        "output_tokens": output,
        "cache_read_tokens": cache_read,
        "cache_write_tokens": cache_write,
        "reasoning_tokens": reasoning,
        "cost": cost,
    })
}

/// `base` with the fields of `extra` added.
pub fn with(mut base: Value, extra: Value) -> Value {
    let fields = base.as_object_mut().expect("an object");
    for (name, value) in extra.as_object().expect("an object") {
        fields.insert(name.clone(), value.clone());
    }
    base
}

/// An audit record with small but complete content.
pub fn audit_record(audit_id: &str, event_number: u128, time: &str) -> AuditRecord {
    let request_body = json!({
        "model": "@planner",
        "messages": [{ "role": "user", "content": "Plan the next step." }],
    })
    .to_string();
    AuditRecord {
        audit_id: audit_id.to_owned(),
        event_id: id(event_number),
        timestamp: at(time),
        expires_at: at("2099-01-01T00:00:00Z"),
        organisation_id: "org-a".into(),
        workspace_id: "ws-1".into(),
        key_id: "key-1".into(),
        end_user: "alice".into(),
        session_id: "sess-1".into(),
        trace_id: "trace-1".into(),
        template: "planner".into(),
        model: "claude-haiku".into(),
        mode: "managed".into(),
        client_request: AuditPart {
            method: "POST".into(),
            url: "/v1/chat/completions".into(),
            headers: vec![("content-type".into(), "application/json".into())],
            body_bytes: request_body.len() as u64,
            body: request_body,
            body_encoding: "utf8".into(),
            ..AuditPart::default()
        },
        provider_requests: vec![AuditPart {
            method: "POST".into(),
            url: "https://api.anthropic.com/v1/messages".into(),
            body: "{\"model\":\"claude-haiku\"}".into(),
            body_encoding: "utf8".into(),
            body_bytes: 24,
            ..AuditPart::default()
        }],
        provider_responses: vec![AuditPart {
            status: 200,
            body: "{\"content\":\"ok\"}".into(),
            body_encoding: "utf8".into(),
            body_bytes: 16,
            ..AuditPart::default()
        }],
        client_response: AuditPart {
            status: 200,
            body_encoding: "ref".into(),
            same_as: "provider_responses[0]".into(),
            body_bytes: 16,
            ..AuditPart::default()
        },
        resolution: json!({ "template": "planner", "version": 3 }),
        timeline: json!({ "pre_us": 900 }),
        outcome: json!({ "finish_reason": "stop" }),
        truncated: false,
    }
}
