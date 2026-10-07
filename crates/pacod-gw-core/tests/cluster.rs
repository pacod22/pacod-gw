//! Several instances on one database (runbook section 9): a central management instance and
//! gateway-only instances, as they run on several machines. Limits, concurrency caps and the
//! instance registry must hold across them.

mod common;

use std::time::{Duration, Instant};

use common::{Fixture, TestGw, TestNode, temp_dir};
use pacod_gw_core::app::App;
use pacod_gw_core::bootstrap::{Bootstrap, Role};
use pacod_gw_testkit::mock::{Behavior, MockProvider};
use serde_json::{Value, json};

/// Two synchronisation rounds: what another instance needs to see a change in a counter.
const SYNC: Duration = Duration::from_millis(2500);

/// A management-only instance with the fixture, and gateways `a` and `b`.
async fn cluster() -> (TestGw, TestNode, TestNode, MockProvider, Fixture) {
    let db = common::fresh_database().await;
    let mock = MockProvider::spawn().await;
    let admin = TestGw::start_on(&db, None, |b| b.role = Role::Admin).await;
    let fx = admin.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let a = TestNode::start(&db, |_| {}).await;
    let b = TestNode::start(&db, |_| {}).await;
    (admin, a, b, mock, fx)
}

async fn chat(http: &reqwest::Client, data: &str, key: &str) -> u16 {
    http.post(format!("{data}/v1/chat/completions"))
        .bearer_auth(key)
        .json(&json!({ "model": "openai/m", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .expect("request")
        .status()
        .as_u16()
}

fn by_name(states: &Value, name: &str) -> Value {
    states.as_array().expect("array").iter().find(|s| s["name"] == name).cloned().unwrap_or_else(|| panic!("{name}"))
}

/// Waits for the start of a minute's first two thirds, so that a test of a per-minute limit
/// does not straddle two minutes.
async fn early_in_a_minute() {
    let second = chrono::Utc::now().timestamp() % 60;
    if second > 35 {
        tokio::time::sleep(Duration::from_secs((61 - second) as u64)).await;
    }
}

#[tokio::test]
async fn a_limit_holds_across_gateways_and_a_reset_reaches_them_all() {
    let (admin, a, b, _mock, fx) = cluster().await;
    let limits = format!("/orgs/{}/workspaces/{}/limits", fx.org, fx.ws);
    let limit = admin
        .create(
            &limits,
            json!({ "name": "hourly", "subject": "workspace", "metric": "requests", "window": "hour", "amount": 5.0 }),
        )
        .await
        .id;
    admin.wait_applied().await;
    let http = &admin.http;

    for _ in 0..3 {
        assert_eq!(chat(http, &a.data, &fx.key).await, 200);
    }
    for _ in 0..2 {
        assert_eq!(chat(http, &b.data, &fx.key).await, 200);
    }
    tokio::time::sleep(SYNC).await;

    // The management instance serves no traffic; it reports the stored total.
    let state = format!("/orgs/{}/workspaces/{}/limit-state", fx.org, fx.ws);
    let (st, v) = admin.api("GET", &state, None).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(by_name(&v, "hourly")["used"].as_f64(), Some(5.0), "{v}");
    // Both gateways know the limit is used up.
    assert_eq!(chat(http, &a.data, &fx.key).await, 402);
    assert_eq!(chat(http, &b.data, &fx.key).await, 402);
    // The open alert says so too.
    let (_, alerts) = admin.api("GET", "/monitoring/alerts", None).await;
    assert!(alerts.as_array().unwrap().iter().any(|al| al["id"] == format!("limit:{limit}")), "{alerts}");

    // A reset on the management instance reaches both gateways.
    let (st, v) = admin.api("POST", &format!("{limits}/{limit}/reset"), Some(json!({}))).await;
    assert_eq!(st, 204, "{v}");
    tokio::time::sleep(SYNC).await;
    assert_eq!(chat(http, &a.data, &fx.key).await, 200, "gateway a admits again after the reset");
    assert_eq!(chat(http, &b.data, &fx.key).await, 200, "gateway b admits again after the reset");
}

#[tokio::test]
async fn the_quota_on_one_gateway_counts_what_another_served() {
    let (admin, a, b, _mock, fx) = cluster().await;
    let limits = format!("/orgs/{}/workspaces/{}/limits", fx.org, fx.ws);
    admin
        .create(
            &limits,
            json!({ "name": "per-key", "subject": "key", "metric": "requests", "window": "day", "amount": 10.0 }),
        )
        .await;
    admin.wait_applied().await;
    for _ in 0..3 {
        assert_eq!(chat(&admin.http, &a.data, &fx.key).await, 200);
    }
    tokio::time::sleep(SYNC).await;
    // Gateway b has never served this key.
    let q: Value = admin
        .http
        .get(format!("{}/gw/v1/quota", b.data))
        .bearer_auth(&fx.key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let per_key = by_name(&q["limits"], "per-key");
    assert_eq!((per_key["used"].as_f64(), per_key["remaining"].as_f64()), (Some(3.0), Some(7.0)), "{q}");
}

#[tokio::test]
async fn a_per_minute_limit_spans_gateways() {
    let (admin, a, b, _mock, fx) = cluster().await;
    let limits = format!("/orgs/{}/workspaces/{}/limits", fx.org, fx.ws);
    admin
        .create(&limits, json!({ "name": "throttle", "subject": "workspace", "metric": "requests", "window": "minute", "amount": 4.0 }))
        .await;
    admin.wait_applied().await;
    early_in_a_minute().await;
    for _ in 0..2 {
        assert_eq!(chat(&admin.http, &a.data, &fx.key).await, 200);
        assert_eq!(chat(&admin.http, &b.data, &fx.key).await, 200);
    }
    tokio::time::sleep(SYNC).await;
    assert_eq!(chat(&admin.http, &a.data, &fx.key).await, 429);
    assert_eq!(chat(&admin.http, &b.data, &fx.key).await, 429);
}

#[tokio::test]
async fn a_key_concurrency_cap_is_shared_by_the_gateways() {
    let (admin, a, b, mock, fx) = cluster().await;
    let key = admin.key(&fx, json!({ "name": "pair", "max_concurrency": 2 })).await;
    admin.wait_applied().await;
    // Each gateway counts the gateways that run and takes its share of the cap.
    let deadline = Instant::now() + Duration::from_secs(8);
    for node in [&a, &b] {
        while node.app.gw.data_plane_instances.load(std::sync::atomic::Ordering::Relaxed) != 2 {
            assert!(Instant::now() < deadline, "the gateways did not count each other");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    mock.set_script(Behavior::parse_all(["delay=800"]).unwrap());
    let slow = {
        let (http, data, key) = (admin.http.clone(), a.data.clone(), key.clone());
        tokio::spawn(async move { chat(&http, &data, &key).await })
    };
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(chat(&admin.http, &a.data, &key).await, 429, "gateway a's share of the cap is one call");
    assert_eq!(chat(&admin.http, &b.data, &key).await, 200, "gateway b has a share of its own");
    assert_eq!(slow.await.unwrap(), 200);
}

#[tokio::test]
async fn bad_keys_are_counted_per_client_behind_a_trusted_proxy() {
    let db = common::fresh_database().await;
    let gw = TestGw::start_on(&db, None, |b| {
        b.trusted_proxies = pacod_gw_core::dataplane::client_ip::TrustedProxies::parse(&["127.0.0.0/8".into()]).unwrap()
    })
    .await;
    let bad = |client: &'static str| {
        let (http, url) = (gw.http.clone(), format!("{}/v1/models", gw.data));
        async move {
            http.get(url)
                .bearer_auth("gw-not-a-real-key")
                .header("x-forwarded-for", client)
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    for _ in 0..60 {
        assert_eq!(bad("198.51.100.1").await, 401);
    }
    assert_eq!(bad("198.51.100.1").await, 429, "this client is throttled");
    assert_eq!(bad("198.51.100.2").await, 401, "another client behind the same proxy is not");
}

#[tokio::test]
async fn a_running_instance_id_cannot_be_taken() {
    let db = common::fresh_database().await;
    let a = TestNode::start(&db, |b| b.instance_id = "gateway-1".into()).await;
    let started = Instant::now();
    let mut boot = Bootstrap::for_tests(&db, temp_dir("other"));
    boot.role = Role::Gateway;
    boot.instance_id = "gateway-1".into();
    let err = App::start(boot, None).await.err().expect("refused").to_string();
    assert!(err.contains("in use by a running process"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(5), "noticed within a few heartbeats");
    a.app.shutdown().await;
}

#[tokio::test]
async fn a_restart_after_a_crash_claims_its_id_at_once() {
    let db = common::fresh_database().await;
    let node = TestNode::start(&db, |b| b.instance_id = "gateway-1".into()).await;
    let dir = node.spool_dir.clone();
    node.app.shutdown().await;
    // A crashed process leaves its row behind, still fresh.
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("instance.json")).unwrap()).unwrap();
    let pool = sqlx::PgPool::connect(&db).await.unwrap();
    sqlx::query(
        "INSERT INTO instances (id, role, hostname, software_version, applied_version, started_at, heartbeat_at, boot_id, home_id)
         VALUES ('gateway-1', 'gateway', 'h', '0', 0, now(), now(), 'gone', $1)",
    )
    .bind(stored["home_id"].as_str().unwrap())
    .execute(&pool)
    .await
    .unwrap();

    let started = Instant::now();
    let mut boot = Bootstrap::for_tests(&db, &dir);
    boot.role = Role::Gateway;
    boot.instance_id = "gateway-1".into();
    let app = App::start(boot, None).await.expect("the same spool directory is the same instance");
    assert!(started.elapsed() < Duration::from_secs(2), "no wait for the old row to go stale");
    app.shutdown().await;

    // Without PGW_INSTANCE_ID, the id is the one kept in the directory.
    let mut boot = Bootstrap::for_tests(&db, &dir);
    boot.role = Role::Gateway;
    boot.instance_id = String::new();
    let app = App::start(boot, None).await.expect("starts");
    assert_eq!(app.gw.boot.instance_id, "gateway-1");
    assert_eq!(stored["id"], "gateway-1");
    app.shutdown().await;
}

#[tokio::test]
async fn a_shared_spool_directory_is_refused() {
    let db = common::fresh_database().await;
    let a = TestNode::start(&db, |_| {}).await;
    let mut boot = Bootstrap::for_tests(&db, &a.spool_dir);
    boot.role = Role::Gateway;
    let err = App::start(boot, None).await.err().expect("refused").to_string();
    assert!(err.contains("spool directory"), "{err}");
    a.app.shutdown().await;
}

#[tokio::test]
async fn an_unknown_newer_migration_does_not_stop_an_older_binary() {
    let db = common::fresh_database().await;
    let store = pacod_gw_core::store::Store::connect(&db, 2).await.unwrap();
    store.migrate().await.unwrap();
    sqlx::query(
        "INSERT INTO _sqlx_migrations (version, description, success, checksum, execution_time)
         VALUES (99990101000000, 'from a newer release', true, '\\x00', 0)",
    )
    .execute(store.pool())
    .await
    .unwrap();
    store.migrate().await.expect("a migration this binary does not know is accepted");
}

#[tokio::test]
async fn a_restored_database_is_picked_up_by_running_instances() {
    let gw = TestGw::start(None).await;
    let before = gw.app.gw.config.version();
    gw.create("/orgs", json!({ "name": "made-after-the-backup" })).await;
    assert!(gw.app.gw.config.version() > before);
    // What a restore does: the configuration of an earlier version comes back.
    let pool = sqlx::PgPool::connect(&gw.database_url).await.unwrap();
    sqlx::query("DELETE FROM config_objects WHERE name = 'made-after-the-backup'").execute(&pool).await.unwrap();
    sqlx::query("UPDATE config_head SET version = $1").bind(before).execute(&pool).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while gw.app.gw.config.version() != before {
        assert!(Instant::now() < deadline, "the instance kept the newer snapshot");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(gw.app.gw.config.snapshot().organisations.values().all(|o| o.doc.name != "made-after-the-backup"));
}

#[tokio::test]
async fn a_stopping_gateway_leaves_the_rotation_before_it_closes() {
    let db = common::fresh_database().await;
    let mock = MockProvider::spawn().await;
    let admin = TestGw::start_on(&db, None, |b| b.role = Role::Admin).await;
    let fx = admin.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let node = TestNode::start(&db, |b| b.drain_delay_seconds = 2).await;
    admin.wait_applied().await;
    let (data, http) = (node.data.clone(), admin.http.clone());
    let ready = || {
        let (http, url) = (http.clone(), format!("{data}/readyz"));
        async move { http.get(url).send().await.map(|r| r.status().as_u16()).unwrap_or(0) }
    };
    assert_eq!(ready().await, 200);

    let stopping = tokio::spawn(node.app.shutdown());
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(ready().await, 503, "the load balancer's health check fails first");
    assert_eq!(chat(&http, &data, &fx.key).await, 200, "while calls are still served");
    stopping.await.unwrap();
    assert_eq!(ready().await, 0, "then the listener closes");
}
