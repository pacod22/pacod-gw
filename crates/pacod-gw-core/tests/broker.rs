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
                async move {
                    axum::Json(json!({ "generation": 1, "at": "2026-11-04T00:00:00Z", "total": h.len(), "hits": h }))
                }
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
                async move {
                    let tools = json!([{ "name": "discover_resources" }, { "name": "describe_provider" }]);
                    axum::Json(json!({ "jsonrpc": "2.0", "id": req["id"], "result": { "tools": tools } }))
                }
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
    // Callers see the link in both model lists, where the model has one and nowhere else.
    let mut seen = false;
    for _ in 0..50 {
        let r = gw.http.get(format!("{}/gw/v1/models", gw.data)).bearer_auth(&fx.key).send().await.unwrap();
        let v: Value = r.json().await.unwrap();
        let models = v["models"].as_array().cloned().unwrap_or_default();
        if let Some(m) = models.iter().find(|m| m["id"] == "openai/linked") {
            assert_eq!(m["broker_resource_id"], "res_0b", "{v}");
            let other = models.iter().find(|m| m["id"] == "openai/m").expect("the template's model");
            assert!(other.get("broker_resource_id").is_none(), "{other}");
            let r = gw.http.get(format!("{}/v1/models", gw.data)).bearer_auth(&fx.key).send().await.unwrap();
            let v: Value = r.json().await.unwrap();
            let linked = v["data"].as_array().unwrap().iter().find(|m| m["id"] == "openai/linked").cloned().unwrap();
            assert_eq!(linked["gateway"]["broker_resource_id"], "res_0b", "{linked}");
            seen = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert!(seen, "the linked model reaches the key's model list");
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
    let names: Vec<_> = v["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].clone()).collect();
    assert_eq!(names, [json!("discover_resources")], "a tool that cannot be called is not listed");
    let v: Value =
        mcp(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": { "name": "drop_tables" } }))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
    assert_eq!(v["error"]["code"], -32601);
    // A batch is refused whole: none of its members reaches the broker.
    let v: Value =
        mcp(json!([{ "jsonrpc": "2.0", "id": 3, "method": "tools/list" }])).await.unwrap().json().await.unwrap();
    assert_eq!(v["error"]["code"], -32600);
    assert_eq!(cat.mcp.lock().len(), 1, "only the allowed request reached the broker");
    let r = gw.http.post(format!("{}/mcp", gw.data)).json(&json!({ "method": "tools/list" })).send().await.unwrap();
    assert_eq!(r.status(), 401, "a gateway key is needed");
    // A body is read only once the key is known, and only so far.
    let big = vec![b' '; 65 << 10];
    let r = gw.http.post(format!("{}/mcp", gw.data)).body(big.clone()).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let r = gw.http.post(format!("{}/mcp", gw.data)).bearer_auth(&fx.key).body(big).send().await.unwrap();
    assert_eq!(r.status(), 413);
    // No stream is offered: GET is not allowed, and says what is.
    let r = gw.http.get(format!("{}/mcp", gw.data)).bearer_auth(&fx.key).send().await.unwrap();
    assert_eq!(r.status(), 405);
    assert_eq!(r.headers()["allow"], "POST");
}

/// Every call of one of the broker's tools leaves a record of who called it, with what verdict
/// and how it went; the arguments are a keyed hash, and the handshake and the list leave none.
#[tokio::test]
async fn the_brokers_tool_calls_are_recorded_and_their_arguments_are_not() {
    let mock = MockProvider::spawn().await;
    let cat = Catalogue::default();
    let broker = catalogue_broker(cat.clone()).await;
    let sink = IngestSink::start().await;
    let db = fresh_database().await;
    let gw = TestGw::start_on(&db, Some(&sink), |b| {
        b.broker = Some(BrokerLink { url: broker, token: None, pool_seconds: 300 })
    })
    .await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let mcp = |body: Value| {
        gw.http
            .post(format!("{}/mcp", gw.data))
            .bearer_auth(&fx.key)
            .header("mcp-protocol-version", "2025-11-25")
            .header("x-gw-agent-id", "scout")
            .header("x-gw-trace", "trace-mcp")
            .header("x-gw-tags", "purpose=find,undeclared=x")
            .json(&body)
            .send()
    };
    mcp(json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list" })).await.unwrap();
    let args = json!({ "query": { "kind": "model" }, "limit": 5 });
    let call = json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                       "params": { "name": "discover_resources", "arguments": args } });
    assert_eq!(mcp(call.clone()).await.unwrap().status(), 200);
    // The same arguments with their members in another order.
    let reordered = json!({ "limit": 5, "query": { "kind": "model" } });
    let again = json!({ "jsonrpc": "2.0", "id": 3, "method": "tools/call",
                        "params": { "name": "discover_resources", "arguments": reordered } });
    mcp(again).await.unwrap();
    mcp(json!({ "jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": { "name": "drop_tables" } }))
        .await
        .unwrap();

    let records = sink.wait_tool_calls(3).await;
    assert_eq!(records.len(), 3, "the list leaves no record: {records:?}");
    let (first, second, refused) = (&records[0], &records[1], &records[2]);
    assert_eq!((first.workspace_id.as_str(), first.key_id.as_str()), (fx.ws.as_str(), fx.key_id.as_str()));
    assert_eq!((first.protocol.as_str(), first.protocol_version.as_str()), ("mcp", "2025-11-25"));
    assert_eq!((first.target.as_str(), first.operation.as_str()), ("broker", "discover_resources"));
    assert_eq!((first.verdict.as_str(), first.status.as_str(), first.http_status), ("allowed", "ok", 200));
    assert_eq!((first.agent_id.as_str(), first.trace_id.as_str()), ("scout", "trace-mcp"));
    assert_eq!(first.tags.get("purpose").map(String::as_str), Some("find"));
    assert!(!first.tags.contains_key("undeclared"), "only declared tags are kept");
    assert!(first.request_bytes > 0 && first.response_bytes > 0 && first.total_ms > 0.0);
    // A keyed hash of the arguments, alike whatever the order of their members; never the
    // arguments themselves.
    assert!(first.arguments_hmac.starts_with("hmac1:"), "{}", first.arguments_hmac);
    assert_eq!(first.arguments_hmac, second.arguments_hmac);
    let line = serde_json::to_string(first).unwrap();
    assert!(!line.contains("\"kind\""), "the record holds no argument: {line}");
    // A tool the gateway does not pass: denied before the broker, with the error it was answered.
    assert_eq!((refused.verdict.as_str(), refused.status.as_str()), ("denied", "rejected"));
    assert_eq!((refused.operation.as_str(), refused.rpc_error_code), ("drop_tables", -32601));
    assert!(refused.arguments_hmac.is_empty(), "it had no arguments");
    assert_eq!(cat.mcp.lock().len(), 3, "the list and the two allowed calls reached the broker");
}

#[tokio::test]
async fn an_organisations_template_takes_members_from_the_broker_for_all_its_workspaces() {
    let mock = MockProvider::spawn().await;
    let upstream = format!("{}/v1", mock.base_url());
    let cat = Catalogue::default();
    let broker = catalogue_broker(cat.clone()).await;
    let db = fresh_database().await;
    let gw = TestGw::start_on(&db, None, |b| b.broker = Some(BrokerLink { url: broker, token: None, pool_seconds: 1 }))
        .await;
    let fx = gw.fixture("openai", &upstream).await;
    *cat.org.lock() = fx.org.clone();
    let linked =
        gw.model(&fx, "openai/linked", "linked", json!({ "free": true, "broker_resource_id": "res_0b" })).await;
    *cat.hits.lock() = vec![hit("res_0b", &upstream, "linked")];
    let org_templates = format!("/orgs/{}/templates", fx.org);

    // follow: one record for the organisation, resolved against the organisation's grants.
    let follow = gw
        .create(
            &org_templates,
            json!({ "name": "shared", "spec": { "model": "openai/m",
                    "pool_from": { "query": { "kind": "model" }, "mode": "follow", "max": 2 } } }),
        )
        .await;
    let path = format!("{org_templates}/{}/broker-pool", follow.id);
    let v = eventually_value(&gw, &path, |v| v["in_force"].as_array().is_some_and(|a| a.len() == 2)).await;
    assert_eq!(v["in_force"], json!(["openai/m", "openai/linked"]), "the template's own model stays first");
    assert_eq!(v["pool"]["resources"]["openai/linked"], "res_0b", "{v}");
    // The workspace's own endpoint is for its own templates.
    let (st, _) = gw
        .api("GET", &format!("/orgs/{}/workspaces/{}/templates/{}/broker-pool", fx.org, fx.ws, follow.id), None)
        .await;
    assert_eq!(st, 404);

    // propose: nothing joins until an organisation admin accepts.
    let propose = gw
        .create(
            &org_templates,
            json!({ "name": "proposed", "spec": { "model": "openai/m",
                    "pool_from": { "query": { "kind": "model" }, "max": 2 } } }),
        )
        .await;
    let path = format!("{org_templates}/{}/broker-pool", propose.id);
    let v = eventually_value(&gw, &path, |v| v["pool"]["proposed"].is_array()).await;
    assert_eq!(v["in_force"], json!(["openai/m"]));
    let (st, v) = gw.api("POST", &format!("{path}/accept"), None).await;
    assert_eq!(st, 200, "{v}");
    eventually_value(&gw, &path, |v| v["in_force"].as_array().is_some_and(|a| a.len() == 2)).await;
    let (st, _) = gw.api("POST", &format!("{path}/accept"), None).await;
    assert_eq!(st, 409, "nothing is proposed any more");

    // In a workspace granted the broker's member alone, the inherited template is served by it:
    // the member joined the workspace's copy, and the workspace's grants apply on the call.
    gw.create(&format!("/orgs/{}/workspaces/{}/grants", fx.org, fx.ws), json!({ "model_id": linked })).await;
    let r = gw
        .http
        .post(format!("{}/v1/chat/completions", gw.data))
        .bearer_auth(&fx.key)
        .json(&json!({ "model": "@shared", "messages": [{ "role": "user", "content": "hi" }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap_or_default());
    let sent = mock.captured().last().and_then(|c| c.json().ok()).expect("the call reached the provider");
    assert_eq!(sent["model"], "linked", "{sent}");

    // A template's record goes with it: nothing else would remove it, and it would keep the
    // organisation from being deleted.
    let (_, status) = gw.api("GET", "/broker/status", None).await;
    assert_eq!(status["pools"], 2, "{status}");
    let (st, v) = gw.api("DELETE", &format!("{org_templates}/{}", follow.id), None).await;
    assert_eq!(st, 204, "{v}");
    eventually_value(&gw, "/broker/status", |v| v["pools"] == 1).await;
}

/// A user with one role, signed in.
async fn signed_in(gw: &TestGw, email: &str, role: &str, scope: Value) -> String {
    let u = gw.create("/users", json!({ "email": email, "password": "a long enough password" })).await;
    gw.create("/role-bindings", json!({ "principal": u.id, "role": role, "scope": scope })).await;
    gw.login(email, "a long enough password").await
}

fn named<'a>(v: &'a Value, list: &str, field: &str, value: &str) -> &'a Value {
    v[list].as_array().and_then(|a| a.iter().find(|x| x[field] == value)).unwrap_or_else(|| panic!("{value}: {v}"))
}

#[tokio::test]
async fn an_organisation_offers_several_models_and_templates_at_once() {
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let upper = gw.model(&fx, "openai/GPT-Upper", "gpt-upper", json!({ "free": true })).await;
    let legal = gw.template(&fx, "legal", json!({ "model": "openai/m" })).await;
    let triage = gw
        .create(&format!("/orgs/{}/templates", fx.org), json!({ "name": "triage", "spec": { "model": "openai/m" } }))
        .await;
    // A second workspace, narrowed to one model by grants of its own.
    let lab = gw.create(&format!("/orgs/{}/workspaces", fx.org), json!({ "name": "lab" })).await.id;
    gw.create(&format!("/orgs/{}/workspaces/{lab}/grants", fx.org), json!({ "model_id": "openai/m" })).await;
    let path = format!("/orgs/{}/broker-offer", fx.org);

    let (st, v) = gw.api("GET", &path, None).await;
    assert_eq!(st, 200, "{v}");
    let m = named(&v, "models", "name", "openai/m");
    assert_eq!((&m["offer"], &m["key"]), (&Value::Null, &json!("model:openai/m")), "{m}");
    assert_eq!(m["reached_by"].as_array().map(Vec::len), Some(2), "both workspaces may call it: {m}");
    assert_eq!(named(&v, "models", "name", "openai/gpt-test")["reached_by"], json!([fx.ws]), "lab is narrowed");
    assert_eq!(named(&v, "templates", "key", "org:triage")["reached_by"].as_array().map(Vec::len), Some(2));
    assert_eq!(named(&v, "templates", "key", "prod.legal")["workspace"]["name"], "prod");

    // Two models and two templates in one change.
    let change = json!({
        "models": [
            { "model_id": "openai/m", "offer": { "visibility": "platform" } },
            { "model_id": upper, "offer": { "visibility": "organisation", "summary": "Upper" } },
        ],
        "templates": [
            { "id": legal.id, "offer": { "visibility": "public" } },
            { "id": triage.id, "offer": { "visibility": "organisation" } },
        ],
    });
    let (st, v) = gw.api("POST", &path, Some(change.clone())).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(named(&v, "models", "name", "openai/m")["offer"]["visibility"], "platform");
    assert_eq!(named(&v, "models", "name", "openai/GPT-Upper")["key"], "model:openai/gpt-upper", "lower case");
    assert_eq!(named(&v, "templates", "key", "prod.legal")["current_version"], 2);
    assert_eq!(named(&v, "templates", "key", "org:triage")["offer"]["visibility"], "organisation");
    // Each offer is stored where it always was: the template's switch, the grant.
    let legal_path = format!("/orgs/{}/workspaces/{}/templates/{}", fx.org, fx.ws, legal.id);
    let (_, t) = gw.api("GET", &legal_path, None).await;
    assert_eq!(t["spec"]["broker_offer"]["visibility"], "public", "{t}");
    assert_eq!(t["versions"][0]["comment"], "offered on pacod-broker", "{t}");
    let (_, grants) = gw.api("GET", &format!("/orgs/{}/grants", fx.org), None).await;
    let grant = named(&grants, "items", "model_id", &upper).clone();
    assert_eq!(grant["broker_offer"]["summary"], "Upper", "{grant}");

    // The same change again writes nothing: no empty template versions.
    let (st, v) = gw.api("POST", &path, Some(change)).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(named(&v, "templates", "key", "prod.legal")["current_version"], 2);

    // A null withdraws.
    let (st, v) = gw
        .api(
            "POST",
            &path,
            Some(json!({ "models": [{ "model_id": "openai/m", "offer": null }], "templates": [{ "id": legal.id, "offer": null }] })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(named(&v, "models", "name", "openai/m")["offer"], Value::Null);
    let t = named(&v, "templates", "key", "prod.legal");
    assert_eq!((&t["offer"], &t["current_version"]), (&Value::Null, &json!(3)), "{t}");

    // One item that cannot change fails the whole change, and nothing is written.
    for bad in [
        json!({ "models": [{ "model_id": "openai/gpt-test", "offer": { "visibility": "platform" } }],
                "templates": [{ "id": "tpl_nope", "offer": null }] }),
        json!({ "models": [{ "model_id": "openai/gpt-test", "offer": { "visibility": "everyone" } }] }),
        json!({ "models": [{ "model_id": "openai/gpt-test", "offer": null }, { "model_id": "openai/gpt-test", "offer": null }] }),
        json!({ "models": [{ "model_id": "openai/not-granted", "offer": { "visibility": "platform" } }] }),
    ] {
        let (st, v) = gw.api("POST", &path, Some(bad.clone())).await;
        assert_eq!(st, 422, "{bad}: {v}");
    }
    let (_, v) = gw.api("GET", &path, None).await;
    assert_eq!(named(&v, "models", "name", "openai/gpt-test")["offer"], Value::Null, "nothing was written");

    // A model whose name differs only in case would share the offered one's key; one whose
    // name is no broker key cannot be offered. The grant's own endpoint refuses the same.
    let lower = gw.model(&fx, "openai/gpt-upper", "gpt-upper-2", json!({ "free": true })).await;
    let odd = gw.model(&fx, "_odd/x", "x", json!({ "free": true })).await;
    for id in [&lower, &odd] {
        let (st, v) = gw
            .api("POST", &path, Some(json!({ "models": [{ "model_id": id, "offer": { "visibility": "platform" } }] })))
            .await;
        assert_eq!((st, &v["error"]["details"]["code"]), (422, &json!("broker_offer_model_name")), "{v}");
        let (_, grants) = gw.api("GET", &format!("/orgs/{}/grants", fx.org), None).await;
        let g = named(&grants, "items", "model_id", id).clone();
        let gid = g["id"].as_str().unwrap();
        let (st, v) = gw
            .api(
                "PUT",
                &format!("/orgs/{}/grants/{gid}", fx.org),
                Some(json!({ "model_id": id, "broker_offer": { "visibility": "platform" } })),
            )
            .await;
        assert_eq!(st, 422, "{v}");
    }

    // An organisation admin sees what is offered but leaves the change to the platform, whose
    // decision the grants are; gw-broker's role reads it.
    let admin = signed_in(&gw, "oa@example.test", "org_admin", json!({ "type": "organisation", "id": fx.org })).await;
    let (st, _) = gw.api_as(&admin, "GET", &path, None).await;
    assert_eq!(st, 200);
    let (st, _) = gw.api_as(&admin, "POST", &path, Some(json!({ "models": [] }))).await;
    assert_eq!(st, 403);
    let bridge = signed_in(&gw, "bridge@example.test", "broker_bridge", json!({ "type": "platform" })).await;
    let (st, v) = gw.api_as(&bridge, "GET", &path, None).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(named(&v, "models", "name", "openai/GPT-Upper")["offer"]["visibility"], "organisation");
}
