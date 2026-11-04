//! pacod-broker in the core (runbook section 20): the broker ticket and the time to first token
//! on the usage event, the model's broker link, who may read the broker's calls, and the broker
//! as a source of discovery's proposals.

mod common;

use axum::Router;
use axum::routing::get;
use common::{IngestSink, TestGw, fresh_database};
use pacod_gw_core::bootstrap::BrokerLink;
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

async fn stream(gw: &TestGw, key: &str, ticket: Option<&str>) {
    let mut r = gw.http.post(format!("{}/v1/chat/completions", gw.data)).bearer_auth(key).json(
        &json!({ "model": "openai/m", "stream": true, "messages": [{ "role": "user", "content": "a b c d e f" }] }),
    );
    if let Some(t) = ticket {
        r = r.header("x-gw-broker-ticket", t);
    }
    let r = r.send().await.unwrap();
    assert_eq!(r.status(), 200);
    let _ = r.bytes().await;
}

#[tokio::test]
async fn a_quoted_ticket_and_the_first_token_reach_the_usage_event() {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;

    stream(&gw, &fx.key, Some("tkt_eyJjIjoiYSJ9.c2ln")).await;
    // Anything that is not a broker ticket is not kept.
    stream(&gw, &fx.key, Some("<script>")).await;
    let events = sink.wait_events(2).await;
    let quoted = events.iter().find(|e| !e.broker_ticket.is_empty()).expect("the ticket is on the event");
    assert_eq!(quoted.broker_ticket, "tkt_eyJjIjoiYSJ9.c2ln");
    assert_eq!(events.iter().filter(|e| e.broker_ticket.is_empty()).count(), 1, "the malformed one is dropped");
    for e in &events {
        let t = &e.timing;
        assert!(t.first_token_ms > 0.0, "a streamed answer measures its first token: {t:?}");
        assert!(
            t.first_token_ms + 0.5 >= t.upstream_first_byte_ms,
            "the first token comes with or after the first byte: {t:?}"
        );
    }
}

#[tokio::test]
async fn a_model_carries_its_broker_resource() {
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let id = gw
        .draft_model(&fx.provider, "openai/linked", "linked", json!({ "free": true, "broker_resource_id": "res_0a1b" }))
        .await;
    let (_, m) = gw.api("GET", &format!("/models/{id}"), None).await;
    assert_eq!(m["broker_resource_id"], "res_0a1b");
    let (st, v) = gw
        .api("POST", "/models", Some(json!({ "name": "openai/bad", "provider_id": fx.provider, "upstream_id": "bad", "free": true, "broker_resource_id": "not-an-id" })))
        .await;
    assert_eq!(st, 422, "{v}");
}

#[tokio::test]
async fn the_brokers_calls_are_for_the_broker_bridge_role() {
    let gw = TestGw::start(None).await;
    let r = gw.http.get(format!("{}/api/v1/broker/calls", gw.admin)).send().await.unwrap();
    assert_eq!(r.status(), 401, "signed in only");
    // A platform admin holds every permission; without an analytics service it gets no further.
    let (st, _) = gw.api("GET", "/broker/calls", None).await;
    assert_ne!(st, 403);
}

/// A broker that lists three models: one an active provider of the repository serves and the
/// repository lacks, one the repository has without its link, and one of the gateway's own
/// offerings, which must not be proposed back.
async fn fake_broker(upstream: String) -> String {
    let hits = json!([
        { "id": "res_00aa", "resource": { "name": "gpt-new", "summary": "A new model", "tags": [],
            "model": { "upstream_id": "gpt-new", "context_window": 64000 },
            "interfaces": [{ "protocol": "openai-chat", "url": upstream }] },
          "provider": { "name": "openai" }, "price": null, "reputation": null },
        { "id": "res_00bb", "resource": { "name": "m", "tags": [], "model": { "upstream_id": "m" },
            "interfaces": [{ "protocol": "openai-chat", "url": upstream }] },
          "provider": { "name": "openai" }, "price": null, "reputation": { "score": 0.8 } },
        { "id": "res_00cc", "resource": { "name": "legal", "tags": ["gateway", "template"], "model": { "upstream_id": "@legal" },
            "interfaces": [{ "protocol": "openai-chat", "url": upstream }] },
          "provider": { "name": "gw-acme" }, "price": null, "reputation": null },
    ]);
    let app = Router::new().route(
        "/v1/resources",
        get(move |q: axum::extract::Query<std::collections::HashMap<String, String>>| {
            let hits = hits.clone();
            async move {
                let h = if q.get("kind").map(String::as_str) == Some("model") { hits } else { json!([]) };
                axum::Json(json!({ "generation": 1, "at": "2026-11-04T00:00:00Z", "total": h.as_array().unwrap().len(), "hits": h }))
            }
        }),
    );
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await });
    url
}

async fn broker_proposals(gw: &TestGw, want: usize) -> Vec<Value> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let (_, v) = gw.api("GET", "/discovery/proposals?source=broker", None).await;
        let list = v.as_array().cloned().unwrap_or_default();
        if list.len() >= want {
            return list;
        }
        assert!(std::time::Instant::now() < deadline, "{v}");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn the_broker_proposes_what_the_repositorys_providers_serve() {
    let mock = MockProvider::spawn().await;
    let upstream = format!("{}/v1", mock.base_url());
    let broker = fake_broker(upstream.clone()).await;
    let db = fresh_database().await;
    let gw = TestGw::start_on(&db, None, |b| b.broker = Some(BrokerLink { url: broker, token: None, pool_seconds: 1 }))
        .await;
    let fx = gw.fixture("openai", &upstream).await;

    let (st, _) = gw.api("POST", "/discovery/runs", None).await;
    assert_eq!(st, 202);
    let found = broker_proposals(&gw, 2).await;
    assert_eq!(found.len(), 2, "the gateway's own offering is not proposed: {found:?}");
    let new = found.iter().find(|p| p["kind"] == "new").expect("a new model");
    assert_eq!(new["upstream_id"], "gpt-new");
    assert_eq!(new["provider_id"], fx.provider);
    assert_eq!(new["details"]["broker_resource_id"], "res_00aa");
    let link = found.iter().find(|p| p["kind"] == "update").expect("a link for the model the repository has");
    assert_eq!(link["details"]["changes"]["broker_resource_id"]["reported"], "res_00bb");

    // Accepting the new one makes a draft that is the broker's resource.
    let (st, v) = gw
        .api(
            "POST",
            &format!("/discovery/proposals/{}/accept", new["id"].as_str().unwrap()),
            Some(json!({ "naming": "prefixed" })),
        )
        .await;
    assert!(st == 200 || st == 201, "{v}");
    let (_, models) = gw.api("GET", "/models?limit=500", None).await;
    let made =
        models["items"].as_array().unwrap().iter().find(|m| m["upstream_id"] == "gpt-new").expect("the draft").clone();
    assert_eq!(made["broker_resource_id"], "res_00aa");
    assert_eq!(made["state"], "draft");

    // Applying the link sets it on the model the repository had.
    let (st, v) =
        gw.api("POST", &format!("/discovery/proposals/{}/apply", link["id"].as_str().unwrap()), Some(json!({}))).await;
    assert_eq!(st, 200, "{v}");
    let (_, models) = gw.api("GET", "/models?limit=500", None).await;
    let m = models["items"].as_array().unwrap().iter().find(|m| m["name"] == "openai/m").unwrap().clone();
    assert_eq!(m["broker_resource_id"], "res_00bb");
}

// --- phase 3: pools from the broker, leased upstreams, MCP --------------------------------------

/// A broker whose discovery answers what the test sets, whose providers belong to `org`, and
/// whose MCP server answers the tool list.
#[derive(Clone, Default)]
struct Catalogue {
    hits: std::sync::Arc<parking_lot::Mutex<Vec<Value>>>,
    org: std::sync::Arc<parking_lot::Mutex<String>>,
    mcp: std::sync::Arc<parking_lot::Mutex<Vec<Value>>>,
}

async fn catalogue_broker(c: Catalogue) -> String {
    let (h, o, m) = (c.hits.clone(), c.org.clone(), c.mcp.clone());
    let app = Router::new()
        .route(
            "/v1/discover",
            axum::routing::post(move || {
                let h = h.lock().clone();
                async move { axum::Json(json!({ "generation": 1, "at": "2026-11-04T00:00:00Z", "total": h.len(), "hits": h })) }
            }),
        )
        .route(
            "/v1/providers/{id}",
            get(move |axum::extract::Path(id): axum::extract::Path<String>| {
                let org = o.lock().clone();
                async move { axum::Json(json!({ "id": id, "organisation_id": org })) }
            }),
        )
        .route(
            "/mcp",
            axum::routing::post(move |axum::Json(req): axum::Json<Value>| {
                m.lock().push(req.clone());
                async move { axum::Json(json!({ "jsonrpc": "2.0", "id": req["id"], "result": { "tools": [{ "name": "discover_resources" }] } })) }
            }),
        )
        .route("/v1/resources", get(|| async { axum::Json(json!({ "total": 0, "hits": [] })) }));
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", l.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(l, app).await });
    url
}

fn hit(id: &str, url: &str, upstream: &str) -> Value {
    json!({ "id": id, "provider": { "id": "prv_x", "name": "self-hosted" },
            "resource": { "name": upstream, "tags": [], "model": { "upstream_id": upstream },
                          "interfaces": [{ "protocol": "openai-chat", "url": url }] } })
}

async fn eventually_value(gw: &TestGw, path: &str, ok: impl Fn(&Value) -> bool) -> Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    loop {
        let (_, v) = gw.api("GET", path, None).await;
        if ok(&v) {
            return v;
        }
        assert!(std::time::Instant::now() < deadline, "{path}: {v}");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

#[tokio::test]
async fn pools_from_the_broker_follow_or_propose_and_leased_endpoints_come_and_go() {
    let mock = MockProvider::spawn().await;
    let node = MockProvider::spawn().await;
    let upstream = format!("{}/v1", mock.base_url());
    let cat = Catalogue::default();
    let broker = catalogue_broker(cat.clone()).await;
    let db = fresh_database().await;
    let gw = TestGw::start_on(&db, None, |b| b.broker = Some(BrokerLink { url: broker, token: None, pool_seconds: 1 }))
        .await;
    let fx = gw.fixture("openai", &upstream).await;
    *cat.org.lock() = fx.org.clone();
    gw.model(&fx, "openai/linked", "linked", json!({ "free": true, "broker_resource_id": "res_0b" })).await;
    *cat.hits.lock() =
        vec![hit("res_0b", &upstream, "linked"), hit("res_0c", "https://elsewhere.example/v1", "unknown")];

    // follow: the linked model joins after the template's own.
    let follow = gw
        .template(
            &fx,
            "follows",
            json!({ "model": "openai/m", "pool_from": { "query": { "kind": "model" }, "mode": "follow", "max": 2 } }),
        )
        .await;
    let path = format!("/orgs/{}/workspaces/{}/templates/{}/broker-pool", fx.org, fx.ws, follow.id);
    let v = eventually_value(&gw, &path, |v| v["in_force"].as_array().is_some_and(|a| a.len() == 2)).await;
    assert_eq!(v["in_force"], json!(["openai/m", "openai/linked"]), "the template's own model stays first");
    assert!(v["pool"]["problems"][0].as_str().unwrap().contains("not active models"), "{v}");

    // propose: nothing joins until accepted.
    let propose = gw
        .template(
            &fx,
            "proposes",
            json!({ "model": "openai/m", "pool_from": { "query": { "kind": "model" }, "max": 2 } }),
        )
        .await;
    let path = format!("/orgs/{}/workspaces/{}/templates/{}/broker-pool", fx.org, fx.ws, propose.id);
    let v = eventually_value(&gw, &path, |v| v["pool"]["proposed"].is_array()).await;
    assert_eq!(v["in_force"], json!(["openai/m"]));
    let (st, _) = gw.api("POST", &format!("{path}/accept"), None).await;
    assert_eq!(st, 200);
    eventually_value(&gw, &path, |v| v["in_force"].as_array().is_some_and(|a| a.len() == 2)).await;

    // own_leases: the organisation's self-hosted node becomes a provider and a model, and serves.
    let node_url = format!("{}/v1", node.base_url());
    *cat.hits.lock() = vec![hit("res_0d0d", &node_url, "llama-local")];
    let leased = gw
        .template(
            &fx,
            "local",
            json!({ "model": "openai/m", "pool_from": { "query": { "kind": "model" }, "mode": "follow", "own_leases": true, "max": 2 } }),
        )
        .await;
    let path = format!("/orgs/{}/workspaces/{}/templates/{}/broker-pool", fx.org, fx.ws, leased.id);
    let v = eventually_value(&gw, &path, |v| v["in_force"].as_array().is_some_and(|a| a.len() == 2)).await;
    let member = v["in_force"][1].as_str().unwrap().to_string();
    assert!(member.starts_with("leased-0d0d/"), "{v}");
    let (_, providers) = gw.api("GET", "/providers?limit=500", None).await;
    let p = providers["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "leased-0d0d")
        .expect("a provider for the node")
        .clone();
    assert_eq!(p["base_url"], node_url);
    // A call to that model reaches the node.
    let r = gw
        .http
        .post(format!("{}/v1/chat/completions", gw.data))
        .bearer_auth(&fx.key)
        .json(&json!({ "model": member, "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap_or_default());
    assert!(!node.captured().is_empty(), "the node served the call");

    // The lease ends: the provider is disabled and the member leaves the pool.
    cat.hits.lock().clear();
    eventually_value(&gw, &path, |v| v["in_force"].as_array().is_some_and(|a| a.len() == 1)).await;
    let (_, providers) = gw.api("GET", "/providers?limit=500", None).await;
    let p = providers["items"].as_array().unwrap().iter().find(|p| p["name"] == "leased-0d0d").unwrap().clone();
    assert_eq!(p["status"], "disabled");

    // MCP: the broker's discovery tools behind the gateway key, nothing else.
    let mcp = |body: Value| gw.http.post(format!("{}/mcp", gw.data)).bearer_auth(&fx.key).json(&body).send();
    let v: Value =
        mcp(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await.unwrap().json().await.unwrap();
    assert_eq!(v["result"]["tools"][0]["name"], "discover_resources");
    let v: Value =
        mcp(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "drop_tables" } }))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(v["error"]["code"], -32601);
    assert_eq!(cat.mcp.lock().len(), 1, "only the allowed request reached the broker");
    let r = gw.http.post(format!("{}/mcp", gw.data)).json(&json!({ "method": "tools/list" })).send().await.unwrap();
    assert_eq!(r.status(), 401, "a gateway key is needed");
}
