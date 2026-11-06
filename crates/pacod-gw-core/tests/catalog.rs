//! The model-data sync from the core's side: catalogues from pacod-aggregator and OpenRouter,
//! the models file, and uploads, applied to the models of a provider whose vendor they list. A
//! server in the test process plays the sources.

mod common;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use common::{TestGw, fresh_database, temp_dir};
use pacod_gw_core::bootstrap::AggregatorLink;
use serde_json::{Value, json};

const TOKEN: &str = "agg-consumer-token";

/// The sources: the aggregator's catalogue (with its token) and an OpenRouter list.
struct Sources {
    url: String,
    catalog: Arc<Mutex<Value>>,
    openrouter: Arc<Mutex<Value>>,
    /// The authorisation each request to the aggregator carried.
    auth: Arc<Mutex<Vec<String>>>,
    /// The authorisation each request to OpenRouter's address carried.
    auth_public: Arc<Mutex<Vec<String>>>,
}

async fn sources() -> Sources {
    let catalog = Arc::new(Mutex::new(json!({ "schema": "pacod.catalog/v1", "models": [] })));
    let openrouter = Arc::new(Mutex::new(json!({ "data": [] })));
    let auth = Arc::new(Mutex::new(Vec::new()));
    let auth_public = Arc::new(Mutex::new(Vec::new()));
    let (c, o, a, ap) = (catalog.clone(), openrouter.clone(), auth.clone(), auth_public.clone());
    let app = Router::new()
        .route(
            "/v1/catalog",
            get(move |headers: HeaderMap| {
                let given = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
                a.lock().unwrap().push(given.clone());
                let body = c.lock().unwrap().clone();
                async move {
                    if given != format!("Bearer {TOKEN}") {
                        return StatusCode::UNAUTHORIZED.into_response();
                    }
                    axum::Json(body).into_response()
                }
            }),
        )
        .route(
            "/openrouter/models",
            get(move |headers: HeaderMap| {
                let given = headers.get("authorization").and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
                ap.lock().unwrap().push(given);
                let body = o.lock().unwrap().clone();
                async move { axum::Json(body) }
            }),
        )
        // More than a source may send.
        .route("/huge", get(|| async { vec![b' '; (32 << 20) + 1] }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Sources { url, catalog, openrouter, auth, auth_public }
}

fn catalog(models: Value) -> Value {
    json!({ "schema": "pacod.catalog/v1", "vendors": [{ "id": "openai", "complete": true }], "models": models })
}

fn entry(api_id: &str, input: f64, output: f64, context: u64) -> Value {
    json!({ "vendor": "openai", "api_id": api_id, "api_id_confirmed": true, "kind": "chat",
            "properties": { "context_window": context, "vision": true },
            "price": { "currency": "USD", "rates": { "input": input, "output": output },
                       "checked_at": "2026-11-01T00:00:00Z" } })
}

async fn gateway(s: &Sources, file: Option<PathBuf>) -> TestGw {
    let db = fresh_database().await;
    TestGw::start_on(&db, None, |boot| {
        boot.catalog.aggregator = Some(AggregatorLink { url: s.url.clone(), token: Some(TOKEN.into()) });
        boot.catalog.models_file = file;
    })
    .await
}

/// A provider at an address no preset knows, whose models are OpenAI's in the catalogues.
async fn provider(gw: &TestGw) -> String {
    let id = gw
        .create(
            "/providers",
            json!({ "name": "openai", "family": "openai", "base_url": "http://127.0.0.1:9/v1", "catalog_vendor": "openai" }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{id}/credentials"), json!({ "label": "shared", "secret": "sk-test" })).await;
    id
}

async fn set_job(gw: &TestGw, changes: Value) {
    let (st, mut job) = gw.api("GET", "/price-job", None).await;
    assert_eq!(st, 200, "{job}");
    for (k, v) in changes.as_object().unwrap() {
        job[k] = v.clone();
    }
    let (st, v) = gw.api("PUT", "/price-job", Some(job)).await;
    assert_eq!(st, 200, "{v}");
}

/// The runs of the model data, newest first.
async fn runs(gw: &TestGw) -> Vec<Value> {
    let (st, v) = gw.api("GET", "/price-job/runs?job=sync&limit=50", None).await;
    assert_eq!(st, 200, "{v}");
    v.as_array().cloned().unwrap_or_default()
}

/// Waits for the next finished run of `job` after the `known` ones.
async fn next_run(gw: &TestGw, job: &str, known: usize) -> Value {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let finished: Vec<Value> =
            runs(gw).await.into_iter().filter(|r| r["job"] == job && !r["finished_at"].is_null()).collect();
        if finished.len() > known {
            return finished[0].clone();
        }
        assert!(Instant::now() < deadline, "no run of {job} finished: {:?}", runs(gw).await);
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn finished_runs(gw: &TestGw, job: &str) -> usize {
    runs(gw).await.iter().filter(|r| r["job"] == job && !r["finished_at"].is_null()).count()
}

/// Asks for a sync and waits for it.
async fn sync(gw: &TestGw) -> Value {
    let known = finished_runs(gw, "catalog_sync").await;
    let (st, v) = gw.api("POST", "/price-job/run", None).await;
    assert_eq!(st, 202, "{v}");
    let run = next_run(gw, "catalog_sync", known).await;
    assert_eq!(run["ok"], true, "{run}");
    run
}

/// Asks for a sync that is to fail, and returns its run.
async fn failed_sync(gw: &TestGw) -> Value {
    let known = finished_runs(gw, "catalog_sync").await;
    let (st, v) = gw.api("POST", "/price-job/run", None).await;
    assert_eq!(st, 202, "{v}");
    let run = next_run(gw, "catalog_sync", known).await;
    assert_eq!(run["ok"], false, "{run}");
    run
}

async fn upload(gw: &TestGw, format: &str, body: Value) -> (u16, Value) {
    let r = gw
        .http
        .post(format!("{}/api/v1/price-job/upload?format={format}", gw.admin))
        .bearer_auth(&gw.token)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap())
}

/// The configuration's newest version, and how many entries the sync wrote to the audit log.
/// Asking for a run is an administrator's action, with an entry of its own under their name.
async fn writes(gw: &TestGw) -> (i64, i64) {
    let (_, status) = gw.api("GET", "/system/status", None).await;
    let audit: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_log WHERE actor = 'model-data'")
        .fetch_one(gw.app.store.pool())
        .await
        .unwrap();
    (status["config_head"].as_i64().unwrap(), audit)
}

async fn open_proposals(gw: &TestGw, source: &str) -> Vec<String> {
    let (st, v) = gw.api("GET", &format!("/discovery/proposals?status=open&source={source}"), None).await;
    assert_eq!(st, 200, "{v}");
    v.as_array().unwrap().iter().map(|p| p["upstream_id"].as_str().unwrap().to_owned()).collect()
}

fn outcome<'a>(run: &'a Value, model: &str) -> &'a Value {
    run["result"]["models"]
        .as_array()
        .and_then(|l| l.iter().find(|m| m["model"] == model))
        .unwrap_or_else(|| panic!("{model} is not in {run}"))
}

/// The rates of the model's price in effect.
async fn price(gw: &TestGw, model_id: &str) -> (f64, f64) {
    let (st, v) = gw.api("GET", &format!("/models/{model_id}/prices"), None).await;
    assert_eq!(st, 200, "{v}");
    let active = v
        .as_array()
        .and_then(|l| l.iter().find(|p| p["status"] == "active"))
        .unwrap_or_else(|| panic!("no price in effect: {v}"));
    (active["input"].as_f64().unwrap(), active["output"].as_f64().unwrap())
}

async fn context(gw: &TestGw, model_id: &str) -> Value {
    gw.api("GET", &format!("/models/{model_id}"), None).await.1["context_window"].clone()
}

#[tokio::test]
async fn a_sync_applies_prices_and_properties_but_not_over_a_pin() {
    let s = sources().await;
    *s.catalog.lock().unwrap() = catalog(json!([
        entry("gpt-a", 2.0, 10.0, 400_000),
        entry("gpt-b", 1.0, 4.0, 128_000),
        entry("gpt-new", 0.5, 1.0, 64_000),
    ]));
    let gw = gateway(&s, None).await;
    let p = provider(&gw).await;
    let a = gw.draft_model(&p, "openai/gpt-a", "gpt-a", json!({})).await;
    // A dated snapshot finds its model.
    let b = gw.draft_model(&p, "openai/gpt-b", "gpt-b-2026-01-01", json!({})).await;
    let (st, v) = gw
        .api("PUT", &format!("/models/{b}/price"), Some(json!({ "currency": "USD", "input": 9.0, "output": 9.0 })))
        .await;
    assert_eq!(st, 201, "{v}");
    let (st, v) = gw.api("POST", &format!("/models/{b}/price/pin"), Some(json!({ "pinned": true }))).await;
    assert_eq!(st, 200, "{v}");

    let (_, job) = gw.api("GET", "/price-job", None).await;
    assert_eq!(job["sync"]["aggregator"]["configured"], true, "{job}");
    assert_eq!(job["sync"]["providers"], json!([p]), "{job}");
    set_job(&gw, json!({ "source": "pacod_aggregator" })).await;
    let run = sync(&gw).await;
    assert_eq!(outcome(&run, "openai/gpt-a")["outcome"], "applied", "{run}");
    assert_eq!(outcome(&run, "openai/gpt-b")["outcome"], "pinned", "{run}");
    assert_eq!(price(&gw, &a).await, (2.0, 10.0));
    assert_eq!(price(&gw, &b).await, (9.0, 9.0), "a pinned price stays");
    assert_eq!(context(&gw, &a).await, 400_000);
    assert!(s.auth.lock().unwrap().iter().all(|t| t == &format!("Bearer {TOKEN}")), "the token goes with each request");

    // A model the catalogue lists and the repository lacks is proposed.
    let (_, proposals) = gw.api("GET", "/discovery/proposals", None).await;
    let found = proposals["items"].as_array().or(proposals.as_array()).cloned().unwrap_or_default();
    assert!(found.iter().any(|x| x["upstream_id"] == "gpt-new" && x["source"] == "catalog"), "{proposals}");
}

#[tokio::test]
async fn a_property_applies_when_the_source_changed_it_so_an_admin_edit_survives() {
    let s = sources().await;
    *s.catalog.lock().unwrap() = catalog(json!([entry("gpt-a", 2.0, 10.0, 400_000)]));
    let gw = gateway(&s, None).await;
    let p = provider(&gw).await;
    let a = gw.draft_model(&p, "openai/gpt-a", "gpt-a", json!({})).await;
    set_job(&gw, json!({ "source": "pacod_aggregator" })).await;
    sync(&gw).await;
    assert_eq!(context(&gw, &a).await, 400_000);

    // An administrator knows better; the source says the same as before, so the edit stands.
    let (st, mut m) = gw.api("GET", &format!("/models/{a}"), None).await;
    assert_eq!(st, 200);
    m["context_window"] = json!(123_456);
    let (st, v) = gw.api("PUT", &format!("/models/{a}"), Some(m)).await;
    assert_eq!(st, 200, "{v}");
    let run = sync(&gw).await;
    assert_eq!(outcome(&run, "openai/gpt-a")["outcome"], "unchanged", "{run}");
    assert!(outcome(&run, "openai/gpt-a").get("properties").is_none(), "{run}");
    assert_eq!(context(&gw, &a).await, 123_456);

    // The source changes it: that applies.
    *s.catalog.lock().unwrap() = catalog(json!([entry("gpt-a", 2.0, 10.0, 500_000)]));
    let run = sync(&gw).await;
    assert_eq!(outcome(&run, "openai/gpt-a")["properties"], json!(["context_window"]), "{run}");
    assert_eq!(context(&gw, &a).await, 500_000);
}

#[tokio::test]
async fn openrouter_prices_apply_only_for_the_vendors_allowed() {
    let s = sources().await;
    *s.openrouter.lock().unwrap() = json!({ "data": [
        { "id": "openai/gpt-a", "context_length": 400000, "architecture": { "input_modalities": ["text"], "output_modalities": ["text"] },
          "pricing": { "prompt": "0.000002", "completion": "0.00001" } },
    ] });
    let gw = gateway(&s, None).await;
    let p = provider(&gw).await;
    let a = gw.draft_model(&p, "openai/gpt-a", "gpt-a", json!({})).await;
    set_job(
        &gw,
        json!({ "source": "openrouter", "openrouter_url": format!("{}/openrouter/models", s.url),
                "openrouter_price_vendors": ["anthropic"] }),
    )
    .await;
    let run = sync(&gw).await;
    assert_eq!(outcome(&run, "openai/gpt-a")["outcome"], "price_not_taken", "{run}");
    assert_eq!(context(&gw, &a).await, 400_000, "properties apply for every vendor");
    set_job(&gw, json!({ "openrouter_price_vendors": ["openai"] })).await;
    let run = sync(&gw).await;
    assert_eq!(outcome(&run, "openai/gpt-a")["outcome"], "applied", "{run}");
    assert_eq!(price(&gw, &a).await, (2.0, 10.0));
    assert!(s.auth.lock().unwrap().is_empty(), "the aggregator was not asked");
    let public = s.auth_public.lock().unwrap().clone();
    assert!(!public.is_empty() && public.iter().all(String::is_empty), "the token stays away: {public:?}");

    // An address an administrator names goes through the egress policy.
    let (_, mut job) = gw.api("GET", "/price-job", None).await;
    job["openrouter_url"] = json!("http://10.1.2.3/models");
    let (st, v) = gw.api("PUT", "/price-job", Some(job)).await;
    assert_eq!(st, 422, "{v}");
}

#[tokio::test]
async fn the_models_file_prices_a_new_draft_and_a_restart_does_not_undo_the_sync() {
    let s = sources().await;
    let dir = temp_dir("models");
    let file = dir.join("models.json");
    std::fs::write(&file, catalog(json!([entry("gpt-a", 3.0, 12.0, 200_000)])).to_string()).unwrap();
    let db = fresh_database().await;
    let start = |file: PathBuf| {
        let (db, url) = (db.clone(), s.url.clone());
        async move {
            TestGw::start_on(&db, None, |boot| {
                boot.catalog.aggregator = Some(AggregatorLink { url, token: Some(TOKEN.into()) });
                boot.catalog.models_file = Some(file);
            })
            .await
        }
    };
    let gw = start(file.clone()).await;
    let known = finished_runs(&gw, "catalog_file").await;
    let p = provider(&gw).await;
    // The provider makes the file due again; its reading records what it lists.
    next_run(&gw, "catalog_file", known).await;
    // A draft takes its price from the file at once, and can be activated.
    let a = gw.draft_model(&p, "openai/gpt-a", "gpt-a", json!({})).await;
    assert_eq!(price(&gw, &a).await, (3.0, 12.0));
    gw.activate(&a).await;

    // The daily sync sets another price; a restart reads the unchanged file and leaves it.
    *s.catalog.lock().unwrap() = catalog(json!([entry("gpt-a", 2.0, 10.0, 400_000)]));
    set_job(&gw, json!({ "source": "pacod_aggregator" })).await;
    sync(&gw).await;
    assert_eq!(price(&gw, &a).await, (2.0, 10.0));
    let files = finished_runs(&gw, "catalog_file").await;
    drop(gw);
    let gw = start(file.clone()).await;
    let run = next_run(&gw, "catalog_file", files).await;
    assert_eq!(outcome(&run, "openai/gpt-a")["outcome"], "kept", "{run}");
    assert_eq!(price(&gw, &a).await, (2.0, 10.0), "the restart did not undo the sync");

    // A changed file applies.
    std::fs::write(&file, catalog(json!([entry("gpt-a", 4.0, 16.0, 200_000)])).to_string()).unwrap();
    let files = finished_runs(&gw, "catalog_file").await;
    drop(gw);
    let gw = start(file.clone()).await;
    next_run(&gw, "catalog_file", files).await;
    assert_eq!(price(&gw, &a).await, (4.0, 16.0));

    // A changed file does not move a pinned price.
    let (st, v) = gw.api("POST", &format!("/models/{a}/price/pin"), Some(json!({ "pinned": true }))).await;
    assert_eq!(st, 200, "{v}");
    std::fs::write(&file, catalog(json!([entry("gpt-a", 5.0, 20.0, 200_000)])).to_string()).unwrap();
    let files = finished_runs(&gw, "catalog_file").await;
    drop(gw);
    let gw = start(file).await;
    let run = next_run(&gw, "catalog_file", files).await;
    assert_eq!(outcome(&run, "openai/gpt-a")["outcome"], "pinned", "{run}");
    assert_eq!(price(&gw, &a).await, (4.0, 16.0));
}

#[tokio::test]
async fn a_second_identical_sync_writes_nothing() {
    let s = sources().await;
    *s.catalog.lock().unwrap() =
        catalog(json!([entry("gpt-a", 2.0, 10.0, 400_000), entry("gpt-b", 1.0, 4.0, 128_000)]));
    let gw = gateway(&s, None).await;
    let p = provider(&gw).await;
    gw.draft_model(&p, "openai/gpt-a", "gpt-a", json!({})).await;
    gw.draft_model(&p, "openai/gpt-b", "gpt-b", json!({})).await;
    set_job(&gw, json!({ "source": "pacod_aggregator" })).await;
    let run = sync(&gw).await;
    assert_eq!(outcome(&run, "openai/gpt-a")["outcome"], "applied", "{run}");
    let before = writes(&gw).await;
    let run = sync(&gw).await;
    assert_eq!(outcome(&run, "openai/gpt-a")["outcome"], "unchanged", "{run}");
    assert_eq!(writes(&gw).await, before, "nothing was written");
}

#[tokio::test]
async fn a_price_a_source_repeats_is_confirmed_as_of_the_sources_own_date() {
    use chrono::{Duration as Days, Utc};
    let s = sources().await;
    let db = fresh_database().await;
    let start = || {
        let (db, url) = (db.clone(), s.url.clone());
        async move {
            TestGw::start_on(&db, None, |boot| {
                boot.catalog.aggregator = Some(AggregatorLink { url, token: Some(TOKEN.into()) });
            })
            .await
        }
    };
    let gw = start().await;
    let p = provider(&gw).await;
    let a = gw.draft_model(&p, "openai/gpt-a", "gpt-a", json!({})).await;
    let days_ago = |n: i64| (Utc::now() - Days::days(n)).to_rfc3339();
    let body = json!({ "currency": "USD", "input": 2.0, "output": 10.0 });
    let (st, v) = gw.api("PUT", &format!("/models/{a}/price"), Some(body)).await;
    assert_eq!(st, 201, "{v}");
    // A price that has stood for 90 days. The API never applies one backwards.
    sqlx::query(
        "UPDATE config_objects SET doc = jsonb_set(doc, '{effective_from}', to_jsonb($1::text)) WHERE kind = 'price'",
    )
    .bind(days_ago(90))
    .execute(gw.app.store.pool())
    .await
    .unwrap();
    drop(gw);
    let gw = start().await;
    let confirmed = async || {
        let (_, v) = gw.api("GET", &format!("/models/{a}/prices"), None).await;
        v.as_array().unwrap().iter().find(|p| p["status"] == "active").unwrap()["confirmed_at"].clone()
    };
    let dated = |days: i64| {
        let mut e = entry("gpt-a", 2.0, 10.0, 400_000);
        e["price"]["checked_at"] = json!(days_ago(days));
        catalog(json!([e]))
    };
    set_job(&gw, json!({ "source": "pacod_aggregator" })).await;

    // Read 60 days ago: the price stands confirmed as of then, stale though it is.
    *s.catalog.lock().unwrap() = dated(60);
    let run = sync(&gw).await;
    assert_eq!(outcome(&run, "openai/gpt-a")["outcome"], "unchanged", "{run}");
    let first = confirmed().await;
    let at: chrono::DateTime<Utc> = serde_json::from_value(first.clone()).expect("confirmed");
    assert!((at - (Utc::now() - Days::days(60))).num_minutes().abs() < 5, "{first}");

    // An older reading does not move it back; a newer one moves it on.
    *s.catalog.lock().unwrap() = dated(70);
    sync(&gw).await;
    assert_eq!(confirmed().await, first);
    *s.catalog.lock().unwrap() = dated(1);
    sync(&gw).await;
    let at: chrono::DateTime<Utc> = serde_json::from_value(confirmed().await).unwrap();
    assert!((at - (Utc::now() - Days::days(1))).num_minutes().abs() < 5, "{at}");
}

#[tokio::test]
async fn the_source_file_reads_the_models_file_again_at_each_sync() {
    let s = sources().await;
    let dir = temp_dir("models-source");
    let file = dir.join("models.json");
    std::fs::write(&file, catalog(json!([entry("gpt-a", 3.0, 12.0, 200_000)])).to_string()).unwrap();
    let gw = gateway(&s, Some(file.clone())).await;
    let known = finished_runs(&gw, "catalog_file").await;
    let p = provider(&gw).await;
    next_run(&gw, "catalog_file", known).await;
    let a = gw.draft_model(&p, "openai/gpt-a", "gpt-a", json!({})).await;
    assert_eq!(price(&gw, &a).await, (3.0, 12.0));
    set_job(&gw, json!({ "source": "file" })).await;

    // The same file: nothing moves, the operator's edits included.
    let run = sync(&gw).await;
    assert_eq!(outcome(&run, "openai/gpt-a")["outcome"], "kept", "{run}");
    // A new one is read by the next sync, without a restart.
    std::fs::write(&file, catalog(json!([entry("gpt-a", 4.0, 16.0, 200_000)])).to_string()).unwrap();
    let run = sync(&gw).await;
    assert_eq!(outcome(&run, "openai/gpt-a")["outcome"], "applied", "{run}");
    assert_eq!(price(&gw, &a).await, (4.0, 16.0));
    assert!(s.auth.lock().unwrap().is_empty(), "no source was asked");
}

#[tokio::test]
async fn a_dismissed_proposal_is_not_proposed_again() {
    let s = sources().await;
    *s.catalog.lock().unwrap() =
        catalog(json!([entry("gpt-new", 0.5, 1.0, 64_000), entry("gpt-other", 0.5, 1.0, 64_000)]));
    let gw = gateway(&s, None).await;
    provider(&gw).await;
    set_job(&gw, json!({ "source": "pacod_aggregator" })).await;
    sync(&gw).await;
    let mut open = open_proposals(&gw, "catalog").await;
    open.sort();
    assert_eq!(open, ["gpt-new", "gpt-other"]);
    let (_, list) = gw.api("GET", "/discovery/proposals?status=open&source=catalog", None).await;
    let id = list.as_array().unwrap().iter().find(|p| p["upstream_id"] == "gpt-new").unwrap()["id"].as_str().unwrap();
    let (st, v) = gw.api("POST", &format!("/discovery/proposals/{id}/dismiss"), None).await;
    assert_eq!(st, 204, "{v}");
    sync(&gw).await;
    assert_eq!(open_proposals(&gw, "catalog").await, ["gpt-other"]);

    // A list that no longer names the other one resolves its proposal: it lists the vendor
    // completely.
    *s.catalog.lock().unwrap() = catalog(json!([entry("gpt-new", 0.5, 1.0, 64_000)]));
    sync(&gw).await;
    assert!(open_proposals(&gw, "catalog").await.is_empty());
}

#[tokio::test]
async fn neither_openrouter_nor_litellm_propose_models() {
    let s = sources().await;
    *s.openrouter.lock().unwrap() =
        json!({ "data": [{ "id": "openai/gpt-new", "pricing": { "prompt": "0.000001" } }] });
    let gw = gateway(&s, None).await;
    provider(&gw).await;
    set_job(&gw, json!({ "source": "openrouter", "openrouter_url": format!("{}/openrouter/models", s.url) })).await;
    sync(&gw).await;
    let (_, list) = gw.api("GET", "/discovery/proposals?status=open", None).await;
    assert_eq!(list.as_array().unwrap().len(), 0, "{list}");
}

#[tokio::test]
async fn a_source_that_sends_too_much_or_too_little_fails_the_run_and_changes_nothing() {
    let s = sources().await;
    *s.catalog.lock().unwrap() = catalog(json!([entry("gpt-a", 2.0, 10.0, 400_000)]));
    let gw = gateway(&s, None).await;
    let p = provider(&gw).await;
    let a = gw.draft_model(&p, "openai/gpt-a", "gpt-a", json!({})).await;
    set_job(&gw, json!({ "source": "pacod_aggregator" })).await;
    sync(&gw).await;

    set_job(&gw, json!({ "source": "litellm", "litellm_url": format!("{}/huge", s.url) })).await;
    let run = failed_sync(&gw).await;
    assert!(run["message"].as_str().unwrap().contains("more than 32 MiB"), "{run}");
    set_job(&gw, json!({ "litellm_url": format!("{}/nowhere", s.url) })).await;
    let run = failed_sync(&gw).await;
    assert!(run["message"].as_str().unwrap().contains("404"), "{run}");
    // Not the list it should be.
    *s.catalog.lock().unwrap() = json!({ "schema": "pacod.catalog/v9" });
    set_job(&gw, json!({ "source": "pacod_aggregator" })).await;
    failed_sync(&gw).await;
    assert_eq!(price(&gw, &a).await, (2.0, 10.0));
}

#[tokio::test]
async fn a_vendor_whose_list_shrank_by_more_than_half_is_left_alone() {
    let s = sources().await;
    let many: Vec<Value> = (0..12).map(|i| entry(&format!("gpt-{i}"), 2.0, 10.0, 400_000)).collect();
    *s.catalog.lock().unwrap() = catalog(json!(many));
    let gw = gateway(&s, None).await;
    let p = provider(&gw).await;
    let a = gw.draft_model(&p, "openai/gpt-0", "gpt-0", json!({})).await;
    set_job(&gw, json!({ "source": "pacod_aggregator" })).await;
    sync(&gw).await;
    assert_eq!(price(&gw, &a).await, (2.0, 10.0));

    *s.catalog.lock().unwrap() = catalog(json!([entry("gpt-0", 9.0, 9.0, 1_000)]));
    let run = sync(&gw).await;
    assert_eq!(outcome(&run, "openai/gpt-0")["outcome"], "skipped", "{run}");
    assert_eq!(price(&gw, &a).await, (2.0, 10.0));
}

#[tokio::test]
async fn a_run_left_open_is_closed_at_the_next_round() {
    let s = sources().await;
    let gw = gateway(&s, None).await;
    let store = pacod_gw_core::store::Store::connect(&gw.database_url, 2).await.unwrap();
    store.job_start_run("catalog_sync", "run_left_open", "schedule", None).await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let open = store.job_state("catalog_sync").await.unwrap().current_run;
        if open.is_none() {
            break;
        }
        assert!(Instant::now() < deadline, "the run stayed open");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let run = runs(&gw).await.into_iter().find(|r| r["id"] == "run_left_open").expect("the run is listed");
    assert_eq!(run["ok"], false, "{run}");
    assert!(run["message"].as_str().unwrap().contains("did not finish"), "{run}");
}

#[tokio::test]
async fn an_upload_waits_for_a_run() {
    let s = sources().await;
    let db = fresh_database().await;
    // No round of upkeep closes the run this test opens.
    let gw = TestGw::start_on(&db, None, |boot| {
        boot.upkeep_seconds = 3600;
        boot.catalog.aggregator = Some(AggregatorLink { url: s.url.clone(), token: Some(TOKEN.into()) });
    })
    .await;
    let p = provider(&gw).await;
    let a = gw.draft_model(&p, "openai/gpt-a", "gpt-a", json!({})).await;
    let file = catalog(json!([entry("gpt-a", 2.0, 10.0, 400_000)]));
    let store = pacod_gw_core::store::Store::connect(&gw.database_url, 2).await.unwrap();

    store.job_start_run("catalog_sync", "run_open", "schedule", None).await.unwrap();
    let (st, v) = upload(&gw, "auto", file.clone()).await;
    assert_eq!(st, 409, "{v}");
    store.job_finish_run("run_open", true, "", &json!({})).await.unwrap();
    let (st, v) = upload(&gw, "auto", file).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(price(&gw, &a).await, (2.0, 10.0));
}

#[tokio::test]
async fn a_round_leaves_the_sync_for_later_while_an_upload_is_applied() {
    let s = sources().await;
    *s.catalog.lock().unwrap() = catalog(json!([entry("gpt-a", 2.0, 10.0, 400_000)]));
    let gw = gateway(&s, None).await;
    set_job(&gw, json!({ "source": "pacod_aggregator" })).await;
    let store = pacod_gw_core::store::Store::connect(&gw.database_url, 2).await.unwrap();
    store.job_start_run("catalog_upload", "run_upload", "upload", None).await.unwrap();
    let (st, v) = gw.api("POST", "/price-job/run", None).await;
    assert_eq!(st, 202, "{v}");
    // A round a second: several go by, and neither start the sync nor close the upload.
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert_eq!(finished_runs(&gw, "catalog_sync").await, 0);
    assert_eq!(store.job_state("catalog_upload").await.unwrap().current_run.as_deref(), Some("run_upload"));
    store.job_finish_run("run_upload", true, "", &json!({})).await.unwrap();
    let run = next_run(&gw, "catalog_sync", 0).await;
    assert_eq!(run["ok"], true, "{run}");
}

#[tokio::test]
async fn an_upload_takes_each_format_it_names() {
    let s = sources().await;
    let gw = gateway(&s, None).await;
    let p = provider(&gw).await;
    let a = gw.draft_model(&p, "openai/gpt-a", "gpt-a", json!({})).await;
    let v1 = catalog(json!([entry("gpt-a", 1.0, 5.0, 100_000)]));
    let openrouter =
        json!({ "data": [{ "id": "openai/gpt-a", "pricing": { "prompt": "0.000002", "completion": "0.00001" } }] });
    let litellm = json!({ "gpt-a": { "litellm_provider": "openai", "mode": "chat", "input_cost_per_token": 0.000003, "output_cost_per_token": 0.000015 } });
    let feed = json!({ "currency": "USD", "models": { "openai/gpt-a": { "input": 4.0, "output": 20.0 } } });
    for (format, body, rates) in [
        ("catalog", v1.clone(), (1.0, 5.0)),
        ("openrouter", openrouter, (2.0, 10.0)),
        ("litellm", litellm, (3.0, 15.0)),
        ("pacod", feed, (4.0, 20.0)),
    ] {
        let (st, v) = upload(&gw, format, body).await;
        assert_eq!(st, 200, "{format}: {v}");
        assert_eq!(price(&gw, &a).await, rates, "{format}");
    }
    // Named wrongly, and named nothing the gateway knows.
    assert_eq!(upload(&gw, "litellm", v1.clone()).await.0, 422);
    assert_eq!(upload(&gw, "yaml", v1).await.0, 422);
}

#[tokio::test]
async fn a_job_saved_by_an_earlier_release_counts_as_saved_in_the_console() {
    use pacod_gw_core::bootstrap::CatalogSeed;
    use pacod_gw_core::model::CatalogSource;
    let db = fresh_database().await;
    let gw = TestGw::start_on(&db, None, |_| {}).await;
    set_job(&gw, json!({ "schedule": "1 2 * * *" })).await;
    // The document as a release before the sync wrote it: none of the sync's fields.
    let fields = ["source", "openrouter_url", "litellm_url", "openrouter_price_vendors", "apply_properties", "origin"];
    sqlx::query("UPDATE config_objects SET doc = doc - $1::text[] WHERE id = 'price_job'")
        .bind(fields.map(String::from).to_vec())
        .execute(gw.app.store.pool())
        .await
        .unwrap();
    drop(gw);

    let seed =
        CatalogSeed { source: Some(CatalogSource::Litellm), schedule: Some("5 2 * * *".into()), ..Default::default() };
    let gw = TestGw::start_on(&db, None, |b| b.catalog.seed = seed).await;
    let (st, job) = gw.api("GET", "/price-job", None).await;
    assert_eq!(st, 200, "{job}");
    assert_eq!(job["origin"], "console", "{job}");
    assert_eq!(job["schedule"], "1 2 * * *", "{job}");
    // The fields it lacked read as their defaults.
    assert_eq!(job["source"], json!(pacod_gw_core::model::PriceJob::default().source), "{job}");
    assert_eq!(job["apply_properties"], true, "{job}");
    assert_eq!(job["openrouter_url"], "https://openrouter.ai/api/v1/models", "{job}");
}

#[tokio::test]
async fn an_upload_applies_any_format_and_waits_for_no_run() {
    let s = sources().await;
    let gw = gateway(&s, None).await;
    let p = provider(&gw).await;
    let a = gw.draft_model(&p, "openai/gpt-a", "gpt-a", json!({})).await;
    // LiteLLM's list, as a file of more than the console's old 2 MB.
    let mut litellm = serde_json::Map::new();
    litellm.insert(
        "gpt-a".into(),
        json!({ "litellm_provider": "openai", "mode": "chat", "input_cost_per_token": 0.000002,
                "output_cost_per_token": 0.00001, "max_input_tokens": 300000 }),
    );
    for i in 0..30_000 {
        litellm.insert(
            format!("azure/padding-{i}"),
            json!({ "litellm_provider": "azure", "mode": "chat", "input_cost_per_token": 0.000001 }),
        );
    }
    let body = Value::Object(litellm).to_string();
    assert!(body.len() > 2 << 20, "{}", body.len());
    let r = gw
        .http
        .post(format!("{}/api/v1/price-job/upload?format=auto", gw.admin))
        .bearer_auth(&gw.token)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    let v: Value = r.json().await.unwrap();
    assert_eq!(status, 200, "{v}");
    assert_eq!(v["models"][0]["outcome"], "applied", "{v}");
    assert_eq!(price(&gw, &a).await, (2.0, 10.0));
    assert_eq!(context(&gw, &a).await, 300_000);

    // Not a file of models: refused with the reason.
    let (st, v) = gw.api("POST", "/price-job/upload", Some(json!({ "something": 1 }))).await;
    assert_eq!(st, 422, "{v}");
}

#[tokio::test]
async fn without_the_aggregator_the_sync_is_skipped_and_says_why() {
    let db = fresh_database().await;
    let gw = TestGw::start_on(&db, None, |_| {}).await;
    set_job(&gw, json!({ "source": "pacod_aggregator" })).await;
    let (_, job) = gw.api("GET", "/price-job", None).await;
    assert!(
        job["problems"].as_array().unwrap().iter().any(|p| p.as_str().unwrap().contains("PGW_AGGREGATOR_URL")),
        "{job}"
    );
    let run = sync(&gw).await;
    assert!(run["message"].as_str().unwrap().contains("PGW_AGGREGATOR_URL"), "{run}");
}

#[tokio::test]
async fn the_environment_sets_the_job_until_an_administrator_saves_it() {
    use pacod_gw_core::bootstrap::CatalogSeed;
    use pacod_gw_core::model::CatalogSource;
    let db = fresh_database().await;
    let seed = |source| CatalogSeed { source: Some(source), schedule: Some("5 2 * * *".into()), ..Default::default() };
    let start = |source| {
        let db = db.clone();
        async move { TestGw::start_on(&db, None, |b| b.catalog.seed = seed(source)).await }
    };
    let gw = start(CatalogSource::Openrouter).await;
    let (_, job) = gw.api("GET", "/price-job", None).await;
    assert_eq!(
        (&job["source"], &job["schedule"], &job["origin"]),
        (&json!("openrouter"), &json!("5 2 * * *"), &json!("env"))
    );
    assert_eq!(job["sync"]["seeded"], json!(["source", "schedule"]), "{job}");
    drop(gw);
    // A changed environment sets it again at the next start.
    let gw = start(CatalogSource::Litellm).await;
    assert_eq!(gw.api("GET", "/price-job", None).await.1["source"], "litellm");
    // Saved in the console, the values are the administrator's from then on.
    set_job(&gw, json!({ "schedule": "9 9 * * *" })).await;
    drop(gw);
    let gw = start(CatalogSource::Openrouter).await;
    let (_, job) = gw.api("GET", "/price-job", None).await;
    assert_eq!(
        (&job["source"], &job["schedule"], &job["origin"]),
        (&json!("litellm"), &json!("9 9 * * *"), &json!("console"))
    );
    assert_eq!(job["sync"]["seeded"], json!([]), "{job}");
}

/// A model at the provider with a price entered by hand, in effect.
async fn priced(gw: &TestGw, provider: &str, name: &str, upstream: &str) -> String {
    let id = gw.draft_model(provider, name, upstream, json!({})).await;
    let rates = json!({ "currency": "USD", "input": 2.0, "output": 10.0 });
    let (st, v) = gw.api("PUT", &format!("/models/{id}/price"), Some(rates)).await;
    assert_eq!(st, 201, "{v}");
    gw.activate(&id).await;
    id
}

/// A proposal as an earlier release of the tracker left it, waiting for approval.
async fn legacy_proposal(gw: &TestGw, model_id: &str, version: u32, input: f64, output: f64) -> String {
    let id = format!("prc_legacy_{model_id}_{version}");
    let doc = json!({ "model_id": model_id, "version": version, "currency": "USD", "input": input, "output": output,
                      "variants": {}, "effective_from": "2026-10-01T00:00:00Z", "source": "feed", "status": "proposed",
                      "evidence_url": "", "evidence_text": "" });
    sqlx::query("INSERT INTO config_objects (id, kind, parent, name, doc) VALUES ($1, 'price', $2, $3, $4)")
        .bind(&id)
        .bind(model_id)
        .bind(version.to_string())
        .bind(&doc)
        .execute(gw.app.store.pool())
        .await
        .unwrap();
    gw.app.gw.config.reload_all().await.unwrap();
    id
}

#[tokio::test]
async fn a_sync_applies_every_valid_price_with_its_evidence_and_confirms_what_stands() {
    let s = sources().await;
    let gw = gateway(&s, None).await;
    let p = provider(&gw).await;
    let manual = priced(&gw, &p, "openai/manual", "manual").await;
    let pinned = priced(&gw, &p, "openai/pinned", "pinned").await;
    priced(&gw, &p, "openai/lonely", "lonely").await;
    let steady = priced(&gw, &p, "openai/steady", "steady").await;
    let moved = priced(&gw, &p, "openai/moved", "moved").await;
    priced(&gw, &p, "openai/wrong", "wrong").await;
    let fresh = gw.draft_model(&p, "openai/fresh", "fresh", json!({})).await;
    let (st, v) = gw.api("POST", &format!("/models/{pinned}/price/pin"), Some(json!({ "pinned": true }))).await;
    assert_eq!(st, 200, "{v}");
    // A proposal an earlier release left waiting is settled by the price that applies now.
    legacy_proposal(&gw, &fresh, 1, 0.5, 2.0).await;

    // Without a date of the source's own: a list read today.
    let undated = |api_id: &str, input: f64, output: f64| {
        let mut e = entry(api_id, input, output, 128_000);
        e["price"].as_object_mut().unwrap().remove("checked_at");
        e["price"]["evidence"] = json!({ "url": "https://prices.example/list.json", "text": "the vendor's words" });
        e
    };
    let list = catalog(json!([
        undated("manual", 3.0, 15.0), // entered by hand: replaced
        undated("pinned", 3.0, 15.0), // pinned: left alone
        undated("steady", 2.2, 10.0), // +10 %
        undated("moved", 4.0, 10.0),  // +100 %: applied all the same
        undated("fresh", 1.0, 4.0),   // a first price
        undated("wrong", -1.0, 4.0),  // a misreading
        undated("nothing", 1.0, 4.0), // not a model of the repository
    ]));
    *s.catalog.lock().unwrap() = list.clone();
    set_job(&gw, json!({ "source": "pacod_aggregator" })).await;
    let run = sync(&gw).await;
    let r = |model: &str| outcome(&run, model).clone();
    assert_eq!(
        (r("openai/manual")["outcome"].as_str(), r("openai/manual")["version"].as_u64()),
        (Some("applied"), Some(2))
    );
    assert_eq!(r("openai/pinned")["outcome"], "pinned");
    let st8 = r("openai/steady");
    assert_eq!(
        (st8["outcome"].as_str(), st8["version"].as_u64(), st8["page"].as_str()),
        (Some("applied"), Some(2), Some("https://prices.example/list.json"))
    );
    assert!((st8["change"].as_f64().unwrap() - 0.1).abs() < 1e-9, "{st8}");
    assert_eq!(
        (r("openai/moved")["outcome"].as_str(), r("openai/moved")["detail"].as_str()),
        (Some("applied"), Some("a change of 100 %"))
    );
    assert_eq!(
        (r("openai/fresh")["outcome"].as_str(), r("openai/fresh")["detail"].as_str()),
        (Some("applied"), Some("the model's first price"))
    );
    assert_eq!(r("openai/wrong")["outcome"], "invalid", "{run}");
    assert_eq!(r("openai/lonely")["outcome"], "not_listed");

    // The prices in effect changed, with their source and evidence.
    let (_, current) = gw.api("GET", "/prices", None).await;
    let price = |name: &str| current.as_array().unwrap().iter().find(|p| p["model"] == name).cloned().unwrap();
    let now = price("openai/steady");
    assert_eq!(
        (now["input"].as_f64(), now["source"].as_str(), now["version"].as_u64()),
        (Some(2.2), Some("feed"), Some(2))
    );
    assert_eq!(
        (now["evidence_url"].as_str(), now["evidence_text"].as_str()),
        (Some("https://prices.example/list.json"), Some("the vendor's words"))
    );
    assert_eq!(price("openai/moved")["input"], 4.0);
    assert_eq!(
        (price("openai/manual")["input"].as_f64(), price("openai/manual")["source"].as_str()),
        (Some(3.0), Some("feed"))
    );
    assert_eq!(
        (price("openai/pinned")["input"].as_f64(), price("openai/pinned")["pinned"].as_bool()),
        (Some(2.0), Some(true))
    );
    assert_eq!(price("openai/wrong")["input"], 2.0, "a misreading changes nothing");
    assert_eq!(price("openai/fresh")["input"], 1.0);
    let (_, versions) = gw.api("GET", &format!("/models/{fresh}/prices"), None).await;
    let left = versions.as_array().unwrap().iter().find(|p| p["evidence_url"] == "" && p["version"] == 1).cloned();
    assert_eq!(left.map(|p| p["status"].clone()), Some(json!("superseded")), "{versions}");

    // Proposals of an earlier release can still be approved or rejected.
    let approve = legacy_proposal(&gw, &moved, 3, 4.0, 10.0).await;
    let reject = legacy_proposal(&gw, &manual, 3, 1.0, 4.0).await;
    let (st, approved) = gw.api("POST", &format!("/price-proposals/{approve}/approve"), None).await;
    assert_eq!(
        (st, approved["status"].as_str(), approved["in_effect"].as_bool()),
        (200, Some("active"), Some(true)),
        "{approved}"
    );
    let (st, rejected) = gw.api("POST", &format!("/price-proposals/{reject}/reject"), None).await;
    assert_eq!((st, rejected["status"].as_str()), (200, Some("rejected")));
    assert_eq!(gw.api("POST", &format!("/price-proposals/{reject}/approve"), None).await.0, 409);

    // Forty days on, a price nobody has looked at is stale. (The prices are aged in the
    // database by hand, and the gateway is told to read everything again.)
    sqlx::query(
        "UPDATE config_objects SET doc = jsonb_set(doc, '{effective_from}', to_jsonb(to_char(now() - interval '40 days', 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"')))
         WHERE kind = 'price' AND doc->>'status' = 'active'",
    )
    .execute(gw.app.store.pool())
    .await
    .unwrap();
    gw.app.gw.config.reload_all().await.unwrap();
    let stale = |alerts: &Value| -> Vec<String> {
        let mut names: Vec<String> = alerts
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["kind"] == "price_stale")
            .map(|a| a["title"].as_str().unwrap().split('"').nth(1).unwrap().to_owned())
            .collect();
        names.sort();
        names
    };
    let (_, alerts) = gw.api("GET", "/monitoring/alerts", None).await;
    assert_eq!(
        stale(&alerts),
        ["openai/lonely", "openai/manual", "openai/moved", "openai/steady", "openai/wrong"],
        "a pinned price is not the sync's to check, a draft is not called: {alerts}"
    );

    // The next sync finds the list unchanged: the prices it names are confirmed and no longer
    // stale, without a new version. The one it does not name stays stale, and so does the one
    // it misstates.
    sync(&gw).await;
    let (_, alerts) = gw.api("GET", "/monitoring/alerts", None).await;
    assert_eq!(stale(&alerts), ["openai/lonely", "openai/wrong"], "{alerts}");
    let (_, versions) = gw.api("GET", &format!("/models/{steady}/prices"), None).await;
    let active = versions.as_array().unwrap().iter().find(|p| p["in_effect"] == true).unwrap();
    assert!(active["confirmed_at"].is_string(), "{active}");
    assert_eq!((active["version"].as_u64(), active["input"].as_f64()), (Some(2), Some(2.2)), "{active}");
    assert_eq!(gw.api("GET", "/price-proposals", None).await.1, json!([]));
}

#[tokio::test]
async fn the_console_is_told_of_a_failed_sync_and_a_tenfold_change() {
    let s = sources().await;
    *s.catalog.lock().unwrap() = catalog(json!([entry("gpt-a", 2.0, 10.0, 400_000)]));
    let gw = gateway(&s, None).await;
    let p = provider(&gw).await;
    let a = gw.draft_model(&p, "openai/gpt-a", "gpt-a", json!({})).await;
    set_job(&gw, json!({ "source": "pacod_aggregator" })).await;
    sync(&gw).await;
    let of = |alerts: &Value, kind: &str| -> Vec<String> {
        alerts
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["kind"] == kind)
            .map(|a| a["id"].as_str().unwrap().to_owned())
            .collect()
    };
    assert!(of(&gw.api("GET", "/monitoring/alerts", None).await.1, "model_data").is_empty());

    *s.catalog.lock().unwrap() = catalog(json!([entry("gpt-a", 30.0, 150.0, 400_000)]));
    sync(&gw).await;
    assert_eq!(price(&gw, &a).await, (30.0, 150.0), "it applies all the same");
    assert_eq!(of(&gw.api("GET", "/monitoring/alerts", None).await.1, "model_data"), ["price_jump:openai/gpt-a"]);

    *s.catalog.lock().unwrap() = json!({ "schema": "pacod.catalog/v9" });
    failed_sync(&gw).await;
    let alerts = gw.api("GET", "/monitoring/alerts", None).await.1;
    assert!(of(&alerts, "model_data").contains(&"model_data_sync".to_owned()), "{alerts}");
}

#[tokio::test]
async fn activating_a_model_prices_it_from_what_the_source_said_or_asks_for_a_sync() {
    use pacod_gw_core::bootstrap::CatalogSeed;
    use pacod_gw_core::model::CatalogSource;
    let s = sources().await;
    *s.catalog.lock().unwrap() = catalog(json!([entry("later", 1.5, 6.0, 128_000)]));
    let db = fresh_database().await;
    let gw = TestGw::start_on(&db, None, |boot| {
        boot.scheduled_catalog = true;
        boot.catalog.aggregator = Some(AggregatorLink { url: s.url.clone(), token: Some(TOKEN.into()) });
        boot.catalog.seed = CatalogSeed { source: Some(CatalogSource::PacodAggregator), ..Default::default() };
    })
    .await;
    let p = provider(&gw).await;
    // The first scheduled sync: it lists a model the repository does not have yet.
    next_run(&gw, "catalog_sync", 0).await;

    // A model the source named is priced the moment it is activated, without a sync: the
    // hand-entered price that let it be activated is replaced.
    let known = finished_runs(&gw, "catalog_sync").await;
    let later = priced(&gw, &p, "openai/later", "later").await;
    assert_eq!(price(&gw, &later).await, (1.5, 6.0));
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(finished_runs(&gw, "catalog_sync").await, known, "no sync was needed");

    // One it did not name asks for a sync of that model alone.
    let unknown = priced(&gw, &p, "openai/unknown", "unknown").await;
    let run = next_run(&gw, "catalog_sync", known).await;
    assert_eq!(run["trigger"], "activation", "{run}");
    assert_eq!(run["models"], json!([unknown]), "{run}");
    assert_eq!(outcome(&run, "openai/unknown")["outcome"], "not_listed", "{run}");
}

async fn run_discovery(gw: &TestGw) {
    let (st, v) = gw.api("POST", "/discovery/runs", None).await;
    assert_eq!(st, 202, "{v}");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (_, runs) = gw.api("GET", "/discovery/runs", None).await;
        let list = runs.as_array().unwrap();
        if list.first().is_some_and(|r| !r["finished_at"].is_null()) {
            return;
        }
        assert!(Instant::now() < deadline, "discovery did not finish: {runs}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test]
async fn a_model_only_the_catalogue_lists_is_proposed_and_the_providers_own_list_takes_it_over() {
    let mock = pacod_gw_testkit::mock::MockProvider::spawn().await;
    let s = sources().await;
    let gw = gateway(&s, None).await;
    let deepseek = gw
        .create(
            "/providers",
            json!({ "name": "deepseek", "family": "openai", "catalog_vendor": "deepseek",
                    "base_url": format!("{}/emulate/deepseek", mock.base_url()) }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{deepseek}/credentials"), json!({ "secret": "sk-test" })).await;
    let of = |id: &str, confirmed: bool, input: f64, vision: bool| {
        json!({ "vendor": "deepseek", "api_id": id, "api_id_confirmed": confirmed, "kind": "chat",
                "display_name": id, "properties": { "context_window": 128000, "vision": vision },
                "price": { "currency": "USD", "rates": { "input": input, "output": input * 4.0 } } })
    };
    *s.catalog.lock().unwrap() = json!({ "schema": "pacod.catalog/v1",
        "vendors": [{ "id": "deepseek", "complete": true }],
        "models": [of("deepseek-flash", true, 0.3, false), of("deepseek-secret", true, 1.0, true),
                   of("DeepSeek Chat", false, 0.5, false)] });
    set_job(&gw, json!({ "source": "pacod_aggregator" })).await;
    sync(&gw).await;

    let open = async || -> Vec<(String, String)> {
        let (_, list) = gw.api("GET", "/discovery/proposals?status=open", None).await;
        let mut found: Vec<(String, String)> = list
            .as_array()
            .unwrap()
            .iter()
            .filter(|p| p["provider_id"] == deepseek.as_str())
            .map(|p| (p["upstream_id"].as_str().unwrap().to_owned(), p["source"].as_str().unwrap().to_owned()))
            .collect();
        found.sort();
        found
    };
    assert_eq!(
        open().await,
        [("deepseek-flash".to_owned(), "catalog".to_owned()), ("deepseek-secret".to_owned(), "catalog".to_owned())],
        "a name no vendor states as an id is not proposed"
    );

    // The provider's own list names one of them: that proposal is the provider's now, and its
    // run resolves none of the catalogue's.
    run_discovery(&gw).await;
    let mine = open().await;
    assert!(mine.contains(&("deepseek-flash".to_owned(), "provider".to_owned())), "{mine:?}");
    assert!(mine.contains(&("deepseek-secret".to_owned(), "catalog".to_owned())), "{mine:?}");

    // Accepted, the catalogue's proposal is a draft with the catalogue's facts, priced at once.
    let (_, list) = gw.api("GET", "/discovery/proposals?status=open&source=catalog", None).await;
    let secret = list.as_array().unwrap().iter().find(|p| p["upstream_id"] == "deepseek-secret").unwrap();
    assert_eq!(secret["details"]["vision"], true, "{secret}");
    let (st, m) =
        gw.api("POST", &format!("/discovery/proposals/{}/accept", secret["id"].as_str().unwrap()), None).await;
    assert_eq!(st, 201, "{m}");
    assert_eq!(price(&gw, m["id"].as_str().unwrap()).await.0, 1.0);
}

#[tokio::test]
async fn accepting_a_proposal_the_sources_cannot_price_asks_for_a_sync_of_the_draft() {
    use pacod_gw_core::bootstrap::CatalogSeed;
    use pacod_gw_core::model::CatalogSource;
    let mock = pacod_gw_testkit::mock::MockProvider::spawn().await;
    let s = sources().await;
    let db = fresh_database().await;
    let gw = TestGw::start_on(&db, None, |boot| {
        boot.scheduled_catalog = true;
        boot.catalog.aggregator = Some(AggregatorLink { url: s.url.clone(), token: Some(TOKEN.into()) });
        boot.catalog.seed = CatalogSeed { source: Some(CatalogSource::PacodAggregator), ..Default::default() };
    })
    .await;
    next_run(&gw, "catalog_sync", 0).await;
    let deepseek = gw
        .create(
            "/providers",
            json!({ "name": "deepseek", "family": "openai", "catalog_vendor": "deepseek",
                    "base_url": format!("{}/emulate/deepseek", mock.base_url()) }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{deepseek}/credentials"), json!({ "secret": "sk-test" })).await;
    let ollama = gw
        .create(
            "/providers",
            json!({ "name": "ollama", "family": "ollama", "base_url": format!("{}/v1", mock.base_url()) }),
        )
        .await
        .id;
    run_discovery(&gw).await;
    let proposal = async |provider: &str, upstream: &str| -> String {
        let (_, list) = gw.api("GET", "/discovery/proposals?status=open", None).await;
        let found =
            list.as_array().unwrap().iter().find(|p| p["provider_id"] == provider && p["upstream_id"] == upstream);
        found.unwrap_or_else(|| panic!("no proposal of {upstream}: {list}"))["id"].as_str().unwrap().to_owned()
    };
    let known = finished_runs(&gw, "catalog_sync").await;
    let id = proposal(&deepseek, "deepseek-flash").await;
    let (st, m) = gw.api("POST", &format!("/discovery/proposals/{id}/accept"), None).await;
    assert_eq!(st, 201, "{m}");
    let run = next_run(&gw, "catalog_sync", known).await;
    assert_eq!((run["trigger"].as_str(), &run["models"]), (Some("activation"), &json!([m["id"]])), "{run}");

    // A model on one's own Ollama server is free: nothing to price.
    let known = finished_runs(&gw, "catalog_sync").await;
    let id = proposal(&ollama, "qwen3:latest").await;
    let (st, m) = gw.api("POST", &format!("/discovery/proposals/{id}/accept"), None).await;
    assert_eq!((st, m["free"].as_bool()), (201, Some(true)), "{m}");
    tokio::time::sleep(Duration::from_secs(3)).await;
    assert_eq!(finished_runs(&gw, "catalog_sync").await, known);
}
