//! Event delivery from the spool to the analytics ingest endpoint.

mod common;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use common::{IngestSink, TestGw, fresh_database, temp_dir};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::json;

async fn call(gw: &TestGw, key: &str, n: usize) {
    for _ in 0..n {
        let r = gw
            .http
            .post(format!("{}/proxy/openai/v1/chat/completions", gw.data))
            .bearer_auth(key)
            .json(&json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }] }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
        let _ = r.bytes().await;
    }
}

#[tokio::test]
async fn traffic_continues_while_analytics_is_down_and_events_arrive_afterwards() {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;

    sink.set_failing(true);
    call(&gw, &fx.key, 25).await;
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(sink.events().is_empty(), "nothing is accepted while the service is down");
    let spool = &gw.app.gw.usage_spool;
    assert!(spool.pending_bytes() > 25 * 200, "the events wait: {} bytes", spool.pending_bytes());
    assert_eq!(gw.http.get(format!("{}/readyz", gw.data)).send().await.unwrap().status(), 200);

    sink.set_failing(false);
    let events = sink.wait_events(25).await;
    assert_eq!(events.len(), 25);
    let ids: HashSet<_> = events.iter().map(|e| e.id).collect();
    assert_eq!(ids.len(), 25, "every call has its own event");

    // Later traffic keeps flowing in new batches.
    call(&gw, &fx.key, 5).await;
    assert_eq!(sink.wait_events(30).await.len(), 30);
    // Delivered events stay on disk until their segment is full, but they no longer wait.
    let waited = Instant::now();
    while spool.pending_bytes() > 0 && waited.elapsed() < Duration::from_secs(5) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(spool.pending_bytes(), 0);
    assert!(spool.bytes() > 30 * 200, "the segment is still there: {} bytes", spool.bytes());
    // A retried batch keeps its id, which is what lets the store deduplicate it.
    let batch_ids: Vec<String> = sink.batches().iter().map(|b| b.batch_id.clone()).collect();
    assert!(batch_ids.iter().all(|id| id.contains('-')));
}

#[tokio::test]
async fn events_written_before_a_restart_are_shipped_after_it() {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let db = fresh_database().await;
    let spool = temp_dir("restart");

    let gw = TestGw::start_on(&db, Some(&sink), |b| b.spool_dir = spool.clone()).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    sink.set_failing(true);
    call(&gw, &fx.key, 10).await;
    // Give the writer time to sync, then stop the gateway with the events still unshipped.
    tokio::time::sleep(Duration::from_millis(300)).await;
    gw.app.shutdown().await;
    assert!(sink.events().is_empty());

    // The same spool directory, a new process.
    sink.set_failing(false);
    let _gw2 = TestGw::start_on(&db, Some(&sink), |b| b.spool_dir = spool.clone()).await;
    let start = Instant::now();
    let events = sink.wait_events(10).await;
    assert_eq!(events.len(), 10, "events survive a restart");
    assert!(start.elapsed() < Duration::from_secs(10));
}

#[tokio::test]
async fn without_analytics_the_gateway_serves_and_keeps_no_spool() {
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    call(&gw, &fx.key, 3).await;
    assert!(!gw.app.gw.usage_spool.enabled());
    // The request log needs analytics and says so.
    let (st, v) = gw.api("GET", "/requests", None).await;
    assert_eq!(st, 503);
    assert!(v["error"]["message"].as_str().unwrap().contains("not configured"));
}

#[tokio::test]
async fn a_full_spool_refuses_only_the_scopes_that_chose_to_fail_closed() {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let db = fresh_database().await;
    let gw = TestGw::start_on(&db, Some(&sink), |b| b.spool_max_bytes = 4096).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let ws_path = format!("/orgs/{}/workspaces/{}", fx.org, fx.ws);
    let send = || {
        gw.http
            .post(format!("{}/proxy/openai/v1/chat/completions", gw.data))
            .bearer_auth(&fx.key)
            .json(&json!({ "model": "m", "messages": [{ "role": "user", "content": "hi" }] }))
            .send()
    };
    let ready = || async { gw.http.get(format!("{}/readyz", gw.data)).send().await.unwrap().status().as_u16() };

    // Analytics is down and the spool fills up; the installation keeps serving.
    sink.set_failing(true);
    let started = Instant::now();
    while !gw.app.gw.usage_spool.is_full() {
        assert!(started.elapsed() < Duration::from_secs(10), "the spool did not fill");
        assert_eq!(send().await.unwrap().status(), 200);
    }
    assert_eq!(send().await.unwrap().status(), 200);
    assert_eq!(ready().await, 200);

    // A workspace that chose to fail closed is refused...
    let (_, mut ws) = gw.api("GET", &ws_path, None).await;
    ws["settings"]["on_spool_full"] = json!("fail_closed");
    assert_eq!(gw.api("PUT", &ws_path, Some(ws.clone())).await.0, 200);
    let r = send().await.unwrap();
    assert_eq!(r.status(), 503);
    assert!(r.text().await.unwrap().contains("event spool is full"));
    // ...while the instance stays in rotation for the others.
    assert_eq!(ready().await, 200);

    // When the installation fails closed, a workspace may still keep serving; the instance
    // reports itself not ready.
    let (_, mut settings) = gw.api("GET", "/settings", None).await;
    settings["on_spool_full"] = json!("fail_closed");
    assert_eq!(gw.api("PUT", "/settings", Some(settings)).await.0, 200);
    let (_, mut ws) = gw.api("GET", &ws_path, None).await;
    ws["settings"]["on_spool_full"] = json!("keep_serving");
    assert_eq!(gw.api("PUT", &ws_path, Some(ws)).await.0, 200);
    assert_eq!(send().await.unwrap().status(), 200);
    assert_eq!(ready().await, 503);
}
