//! The service shell: probes, start-up before ClickHouse is ready, migrations, shutdown.

use pacod_gw_analytics::storage::migrations::MIGRATIONS;
use std::time::Duration;

use pacod_gw_analytics::AppState;
use pacod_gw_analytics::storage::ClickHouseError;
use pacod_gw_events::ingest::USAGE_PATH;
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::support::{MARCH, TestApp, clickhouse_url, config, fixture, ndjson};

async fn probe(app: &TestApp, path: &str) -> (StatusCode, Value) {
    let response = app.http.get(format!("{}{path}", app.base)).send().await.expect("answer");
    let status = response.status();
    (status, response.json().await.expect("JSON"))
}

#[tokio::test]
async fn readiness_follows_the_migrations() {
    // The service is up, ClickHouse answers, but the schema has not been applied.
    let app = TestApp::start_without_migrations().await;
    assert_eq!(probe(&app, "/healthz").await, (StatusCode::OK, json!({ "status": "ok" })));
    assert_eq!(
        probe(&app, "/readyz").await,
        (StatusCode::SERVICE_UNAVAILABLE, json!({ "status": "unavailable", "reason": "migrations_pending" }))
    );

    // Until then everything behind the token answers 503, which the core retries.
    let error = app.error("/reports/overview", StatusCode::SERVICE_UNAVAILABLE).await;
    assert_eq!(error["code"], "not_ready");
    let response = app.post_batch(USAGE_PATH, "early-1", ndjson(&fixture())).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    // The migrations run: the database and its tables appear, and the service is ready.
    let latest = MIGRATIONS.len();
    assert_eq!(app.state.migrate().await.expect("migrations"), latest);
    assert_eq!(probe(&app, "/readyz").await, (StatusCode::OK, json!({ "status": "ready", "schema_version": latest })));
    app.json("/reports/overview").await;
    app.store_usage("early-1", &fixture()).await;

    // The ledger disappears (someone dropped it): not ready again.
    app.command("DROP TABLE schema_migrations SYNC").await;
    assert_eq!(probe(&app, "/readyz").await.0, StatusCode::SERVICE_UNAVAILABLE);
}

#[tokio::test]
async fn an_unreachable_clickhouse_is_reported_and_never_a_success() {
    // The discard port: nothing answers there.
    let app = TestApp::start_against("http://127.0.0.1:9").await;
    assert!(matches!(app.state.migrate().await, Err(ClickHouseError::Transport(_))));
    assert_eq!(probe(&app, "/healthz").await.0, StatusCode::OK);
    assert_eq!(
        probe(&app, "/readyz").await,
        (StatusCode::SERVICE_UNAVAILABLE, json!({ "status": "unavailable", "reason": "clickhouse_unreachable" }))
    );
    let response = app.post_batch(USAGE_PATH, "dead-1", ndjson(&fixture())).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);

    // The storage call itself fails as a transport error, without the URL in its message.
    let error =
        app.clickhouse.insert("usage_events", ndjson(&fixture()).into(), "dead-1").await.expect_err("nothing listens");
    assert!(matches!(error, ClickHouseError::Transport(_)), "{error}");
    assert!(!error.to_string().contains("insert_deduplication_token"));
}

#[tokio::test]
async fn a_rejected_login_is_reported_and_keeps_the_password_out_of_the_error() {
    let mut config = config(&clickhouse_url(), "pgw_test_never_created");
    config.clickhouse_user = Some("default".into());
    config.clickhouse_password = Some("not-the-password".into());
    let state = AppState::new(&config).expect("state");

    let error = state.migrate().await.expect_err("the login is refused");
    assert!(matches!(error, ClickHouseError::Server { status: 401, .. }), "{error}");
    assert!(!error.to_string().contains("not-the-password"));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let address = listener.local_addr().expect("address");
    let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
    let server = tokio::spawn(pacod_gw_analytics::serve(listener, state, async {
        let _ = stopped.await;
    }));

    let response = reqwest::get(format!("http://{address}/readyz")).await.expect("answer");
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body: Value = response.json().await.expect("JSON");
    assert_eq!(body, json!({ "status": "unavailable", "reason": "clickhouse_error" }));

    let _ = stop.send(());
    server.await.expect("server task").expect("graceful stop");
}

#[tokio::test]
async fn migrations_build_the_schema_once() {
    let app = TestApp::start().await;

    let ledger = app.select("SELECT DISTINCT version, name FROM schema_migrations ORDER BY version").await;
    let every: Vec<Value> = MIGRATIONS.iter().map(|m| json!({ "version": m.version, "name": m.name })).collect();
    assert_eq!(ledger, every);
    assert_eq!(ledger[0], json!({ "version": 1, "name": "0001_usage_events" }));

    // A restart finds nothing to apply and keeps the data.
    app.store_usage("fixture-1", &fixture()).await;
    assert_eq!(app.state.migrate().await.expect("migrations"), 0);
    assert_eq!(app.stored_rows("usage_events").await, 9);
    assert_eq!(app.request_numbers(MARCH).await.len(), 8);

    let table = app
        .select(
            "SELECT engine_full FROM system.tables \
             WHERE database = currentDatabase() AND name = 'usage_events'",
        )
        .await;
    let engine = table[0]["engine_full"].as_str().expect("engine");
    for clause in [
        "ReplacingMergeTree",
        "PARTITION BY toYYYYMM(timestamp)",
        "ORDER BY (organisation_id, workspace_id, timestamp, id)",
        "non_replicated_deduplication_window = 10000",
    ] {
        assert!(engine.contains(clause), "`{clause}` is missing in: {engine}");
    }

    let column_type = |name: &'static str| {
        let app = &app;
        async move {
            let rows = app
                .select(&format!(
                    "SELECT type FROM system.columns WHERE database = currentDatabase() \
                     AND table = 'usage_events' AND name = '{name}'"
                ))
                .await;
            rows[0]["type"].as_str().expect("type").to_owned()
        }
    };
    assert_eq!(column_type("tags").await, "Map(String, String)");
    assert_eq!(column_type("cost").await, "Nullable(Float64)");
    assert_eq!(column_type("gateway_delay_ms").await, "Float32");
    assert_eq!(column_type("total_ms").await, "Float32");
    assert_eq!(column_type("provider").await, "LowCardinality(String)");
    assert_eq!(column_type("id").await, "UUID");
}

#[tokio::test]
async fn two_instances_may_migrate_the_same_database_at_once() {
    let app = TestApp::start_without_migrations().await;
    let database = app.clickhouse.database().to_owned();
    let other = AppState::new(&config(&clickhouse_url(), &database)).expect("state");

    let (first, second) = tokio::join!(app.state.migrate(), other.migrate());
    first.expect("first instance");
    second.expect("second instance");

    let ledger = app.select("SELECT DISTINCT version FROM schema_migrations ORDER BY version").await;
    let every: Vec<Value> = MIGRATIONS.iter().map(|m| json!({ "version": m.version })).collect();
    assert_eq!(ledger, every);
    assert_eq!(probe(&app, "/readyz").await.0, StatusCode::OK);
}

#[tokio::test]
async fn shutdown_ends_the_server() {
    let mut app = TestApp::start().await;
    assert_eq!(probe(&app, "/healthz").await.0, StatusCode::OK);

    app.stop();
    let server = app.server.take().expect("server task");
    let served = tokio::time::timeout(Duration::from_secs(10), server)
        .await
        .expect("the server stops in time")
        .expect("the server task does not panic");
    served.expect("serve returns Ok after a graceful shutdown");

    let refused = reqwest::Client::new().get(format!("{}/healthz", app.base)).send().await;
    assert!(refused.is_err(), "the listener is closed");
}

#[test]
fn a_short_token_is_refused_before_anything_starts() {
    let mut bad = config("http://localhost:8123", "pacod_gw");
    bad.internal_token = "too-short".into();
    let error = AppState::new(&bad).err().expect("refused");
    assert!(error.to_string().contains("PGW_INTERNAL_TOKEN"));
}
