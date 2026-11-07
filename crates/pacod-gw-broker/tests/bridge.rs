//! The bridge against a real pacod-broker and a fake of the gateway's management API.
//!
//! The broker is the binary `BROKER_BIN` names (a pacod-broker build, in open mode). Without it
//! the test says so and passes: the broker is another repository, and this one builds without it.

use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::{Path, State};
use axum::routing::get;
use chrono::Utc;
use pacod_broker_client::{Announcement, BrokerClient, Provider, Query};
use pacod_gw_broker::bridge::Bridge;
use pacod_gw_broker::config::{BridgeFile, Config, Offering, OrgOffer};
use pacod_gw_broker::metrics::Metrics;
use parking_lot::Mutex;
use serde_json::{Value, json};

/// What the fake gateway answers; tests change it as the gateway would change.
#[derive(Default)]
struct Fake {
    description: String,
    breaker: String,
    requests: Vec<Value>,
    calls: Vec<Value>,
}

type Shared = Arc<Mutex<Fake>>;

fn page(items: Value) -> axum::Json<Value> {
    axum::Json(json!({ "items": items, "next_cursor": null }))
}

async fn fake_gateway(state: Shared) -> String {
    let app = Router::new()
        .route("/api/v1/orgs", get(|| async { page(json!([{ "id": "org_1", "name": "acme", "display_name": "Acme" }])) }))
        .route("/api/v1/orgs/{org}/workspaces", get(|| async { page(json!([{ "id": "ws_1", "name": "prod" }])) }))
        .route(
            "/api/v1/orgs/{org}/workspaces/{ws}/templates",
            get(|State(s): State<Shared>| async move {
                let d = s.lock().description.clone();
                page(json!([{ "id": "tpl_1", "name": "legal", "description": d, "status": "active", "current_version": 2,
                              "spec": { "model": "openai/gpt-4o" } }]))
            }),
        )
        // The organisation's own templates: one offers itself, and every workspace reaches it.
        .route(
            "/api/v1/orgs/{org}/templates",
            get(|| async {
                page(json!([{ "id": "otp_1", "name": "triage", "description": "Sorts requests", "status": "active",
                              "current_version": 1, "spec": { "model": "openai/gpt-4o", "broker_offer": { "visibility": "public" } } }]))
            }),
        )
        .route(
            "/api/v1/models",
            get(|| async { page(json!([{ "id": "mdl_1", "name": "openai/gpt-4o", "kind": "chat", "state": "active", "context_window": 128000 }])) }),
        )
        .route(
            "/api/v1/models/{id}/prices",
            get(|Path(_id): Path<String>| async {
                axum::Json(json!([{ "model_id": "mdl_1", "version": 1, "currency": "EUR", "effective_from": "2026-01-01T00:00:00Z",
                                    "status": "active", "input": 2.0, "output": 8.0, "model": "openai/gpt-4o", "in_effect": true }]))
            }),
        )
        .route(
            "/api/v1/monitoring/routing",
            get(|State(s): State<Shared>| async move {
                let b = s.lock().breaker.clone();
                axum::Json(json!([{ "instance_id": "i1", "model": "openai/gpt-4o", "provider": "openai", "credential_id": "c1",
                                    "breaker": b, "in_flight": 3, "first_byte_ms": 210.0 }]))
            }),
        )
        .route(
            "/api/v1/broker/calls",
            get(|State(s): State<Shared>| async move {
                let r = s.lock().calls.clone();
                axum::Json(json!({ "requests": r, "next_cursor": null }))
            }),
        )
        .route(
            "/api/v1/requests",
            get(|State(s): State<Shared>| async move {
                let r = s.lock().requests.clone();
                axum::Json(json!({ "requests": r, "next_cursor": null }))
            }),
        )
        .with_state(state);
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await });
    url
}

struct Broker {
    url: String,
    admin: String,
    _child: tokio::process::Child,
}

fn port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port()
}

async fn broker(bin: &str) -> Broker {
    let (p, a) = (port(), port());
    let child = tokio::process::Command::new(bin)
        .arg("serve")
        .env_clear()
        .env("BROKER_LISTEN", format!("127.0.0.1:{p}"))
        .env("BROKER_ADMIN_LISTEN", format!("127.0.0.1:{a}"))
        .env("BROKER_METRICS_LISTEN", "off")
        .env("BROKER_REBUILD_MS", "5")
        .env("BROKER_REPUTATION_EVERY", "1")
        .env("BROKER_LEASE_TTL_MIN", "2")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let url = format!("http://127.0.0.1:{p}");
    for _ in 0..100 {
        if reqwest::get(format!("{url}/healthz")).await.is_ok_and(|r| r.status().is_success()) {
            return Broker { url, admin: format!("http://127.0.0.1:{a}"), _child: child };
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the broker did not start");
}

async fn eventually<F, Fut, T>(what: &str, mut f: F) -> T
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Option<T>>,
{
    for _ in 0..100 {
        if let Some(v) = f().await {
            return v;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("{what} did not happen");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn offerings_live_state_and_evidence_reach_the_broker() {
    let Ok(bin) = std::env::var("BROKER_BIN") else {
        eprintln!("BROKER_BIN is not set: skipped (point it at a pacod-broker build to run this test)");
        return;
    };
    let b = broker(&bin).await;
    let fake: Shared = Arc::new(Mutex::new(Fake {
        description: "Contract review".into(),
        breaker: "closed".into(),
        ..Fake::default()
    }));
    let gw = fake_gateway(fake.clone()).await;

    // An upstream the gateway calls, announced by its own provider, which the bridge links to.
    let upstream: Announcement = serde_json::from_value(json!({
        "ttl_s": 30, "provider": { "name": "openai-like", "summary": "An upstream" },
        "resources": [{ "key": "gpt", "kind": "model", "name": "gpt-4o", "summary": "Upstream model",
                        "interfaces": [{ "protocol": "openai-chat", "url": "https://upstream.example/v1" }],
                        "access": { "visibility": "public" } }]
    }))
    .unwrap();
    let up = Provider::new(BrokerClient::new(&b.url).unwrap(), upstream).start().await.unwrap();
    let upstream_rid = up.announced().await.resources["gpt"].clone();

    let cfg = Config {
        gw_url: gw,
        gw_token: "pgwa_test".into(),
        broker_url: b.url.clone(),
        broker_open: true,
        consumer_token: None,
        public_url: "https://gw.example".into(),
        obtain_url: "https://console.example/keys".into(),
        listen: "127.0.0.1:0".parse().unwrap(),
        refresh: Duration::from_secs(600),
        live_every: Duration::from_secs(1),
        evidence_every: Duration::from_secs(60),
        sample_per_hour: 100,
        ttl_s: 6,
        file: BridgeFile {
            organisations: vec![OrgOffer {
                organisation: "acme".into(),
                token: None,
                token_file: None,
                offer: vec![Offering {
                    workspace: Some("prod".into()),
                    template: "legal".into(),
                    visibility: "public".into(),
                    name: None,
                    summary: None,
                }],
            }],
            links: BTreeMap::from([("openai/gpt-4o".to_string(), upstream_rid.clone())]),
        },
    };
    let metrics = Arc::new(Metrics::default());
    let bridge = Bridge::new(cfg, metrics.clone()).unwrap();

    // The offering appears, with the template's prices and the call name.
    bridge.refresh().await.unwrap();
    let consumer = BrokerClient::new(&b.url).unwrap().with_client_name("someone");
    let q = Query { kind: Some("model".into()), q: Some("legal".into()), ..Query::default() };
    let hit = eventually("the offering", || {
        let (c, q) = (consumer.clone(), q.clone());
        async move { c.discover(&q).await.ok()?.hits.into_iter().next() }
    })
    .await;
    let v = serde_json::to_value(&hit).unwrap();
    assert_eq!(v["resource"]["key"], "prod.legal", "{v}");
    assert_eq!(v["resource"]["summary"], "Contract review");
    assert_eq!(v["resource"]["model"]["upstream_id"], "@legal");
    assert_eq!(v["resource"]["pricing"]["currency"], "EUR");
    assert_eq!(v["provider"]["name"], "gw-acme");
    let rid = hit.id.clone();
    // The organisation's template offers itself beside it, under a key of its own.
    let q = Query { kind: Some("model".into()), q: Some("triage".into()), ..Query::default() };
    let shared = eventually("the organisation's offering", || {
        let (c, q) = (consumer.clone(), q.clone());
        async move { c.discover(&q).await.ok()?.hits.into_iter().next() }
    })
    .await;
    let v = serde_json::to_value(&shared).unwrap();
    assert_eq!(
        (&v["resource"]["key"], &v["resource"]["model"]["upstream_id"]),
        (&json!("org:triage"), &json!("@triage"))
    );

    // Live state: every breaker open makes the offering degraded.
    fake.lock().breaker = "open".into();
    bridge.live().await.unwrap();
    eventually("degraded", || {
        let (c, rid) = (consumer.clone(), rid.clone());
        async move {
            let r = serde_json::to_value(c.resource(&rid).await.ok()?).ok()?;
            (r["live"]["status"] == "degraded").then_some(())
        }
    })
    .await;

    // A change in the gateway updates the offering under the same lease; the id stays.
    fake.lock().description = "Contract review, now in German".into();
    bridge.refresh().await.unwrap();
    eventually("the update", || {
        let (c, rid) = (consumer.clone(), rid.clone());
        async move {
            let r = serde_json::to_value(c.resource(&rid).await.ok()?).ok()?;
            (r["summary"] == "Contract review, now in German").then_some(())
        }
    })
    .await;

    // Evidence: the first round takes a ticket; calls after it are reported in the next.
    bridge.evidence().await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let at = Utc::now().to_rfc3339();
    fake.lock().requests = (0..3)
        .map(|i| {
            json!({ "id": format!("evt_{i}"), "timestamp": at, "status": "ok", "http_status": 200, "attempts": 1,
                    "stream": true, "upstream_first_byte_ms": 230.0, "upstream_stream_ms": 800.0, "output_tokens": 80,
                    "model_served": "openai/gpt-4o" })
        })
        .collect();
    bridge.evidence().await.unwrap();
    assert_eq!(metrics.reports_sent.load(std::sync::atomic::Ordering::Relaxed), 3);
    assert_eq!(metrics.reports_counted.load(std::sync::atomic::Ordering::Relaxed), 3, "the broker counted them");
    let rep = eventually("reputation", || {
        let (admin, rid) = (b.admin.clone(), upstream_rid.clone());
        async move {
            let v: Value =
                reqwest::get(format!("{admin}/api/v1/reputation/resource/{rid}")).await.ok()?.json().await.ok()?;
            let n = v["reputation"]["components"]["reliability"]["samples"].as_f64()?;
            (n >= 2.9).then_some(v)
        }
    })
    .await;
    assert!(rep["reputation"]["components"]["reliability"]["score"].as_f64().unwrap() > 0.5);
    // Nothing twice.
    bridge.evidence().await.unwrap();
    assert_eq!(metrics.reports_sent.load(std::sync::atomic::Ordering::Relaxed), 3);

    // The front door: a consumer quotes the ticket it got for the offering; the bridge checks
    // it as the offering's provider and reports the client's good standing once.
    let ticket = hit.ticket.clone().expect("a ticket for an identified consumer");
    let at = Utc::now().to_rfc3339();
    fake.lock().calls = vec![json!({
        "id": "evt_front", "timestamp": at, "status": "ok", "http_status": 200, "attempts": 1,
        "organisation_id": "org_1", "workspace_id": "ws_1", "template": "legal", "broker_ticket": ticket,
        "stream": false, "upstream_first_byte_ms": 300.0
    })];
    bridge.front_door().await.unwrap();
    assert_eq!(metrics.tickets_seen.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(metrics.client_reports.load(std::sync::atomic::Ordering::Relaxed), 1, "good standing, once");
    bridge.front_door().await.unwrap();
    assert_eq!(metrics.tickets_seen.load(std::sync::atomic::Ordering::Relaxed), 1, "each call once");

    // A call from a workspace to the organisation's template finds that offering.
    let ticket = shared.ticket.clone().expect("a ticket for the organisation's offering");
    let at = Utc::now().to_rfc3339();
    fake.lock().calls.push(json!({
        "id": "evt_shared", "timestamp": at, "status": "ok", "http_status": 200, "attempts": 1,
        "organisation_id": "org_1", "workspace_id": "ws_1", "template": "triage", "broker_ticket": ticket,
        "stream": false, "upstream_first_byte_ms": 300.0
    }));
    bridge.front_door().await.unwrap();
    assert_eq!(metrics.tickets_seen.load(std::sync::atomic::Ordering::Relaxed), 2);
    up.stop().await.unwrap();
}
