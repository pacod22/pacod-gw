//! The scraper against a core of the test's own: it takes the check's lease, fetches a feed,
//! renders pages (with a renderer of the test's), and reports each page and the run.

use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::HeaderMap;
use axum::routing::{get, post};
use axum::{Json, Router};
use pacod_gw_events::egress::EgressPolicy;
use pacod_gw_events::jobs::{BrowserStatus, JOB_PRICE_CHECK, JOB_PRICE_SCAN};
use pacod_gw_scraper::fetch::Fetcher;
use pacod_gw_scraper::render::{BoxFuture, Render, Rendered};
use pacod_gw_scraper::runner::{Core, Runner};
use pacod_gw_scraper::text::fingerprint;
use serde_json::{Value, json};

const TOKEN: &str = "test-internal-token-0123456789";

/// What the fake core was told.
#[derive(Default)]
struct Told {
    leases: Vec<Value>,
    readings: Vec<Value>,
    results: Vec<Value>,
    /// The work of the one run it hands out.
    work: Option<Value>,
}

type Shared = Arc<Mutex<Told>>;

async fn lease(State(told): State<Shared>, headers: HeaderMap, Json(req): Json<Value>) -> Json<Value> {
    assert_eq!(headers["x-gw-internal-token"], TOKEN);
    let mut t = told.lock().unwrap();
    t.leases.push(req.clone());
    match (req["job"].as_str(), t.work.take()) {
        (Some(JOB_PRICE_CHECK), Some(work)) => {
            Json(json!({ "granted": true, "due": true, "run_id": "run_1", "work": work }))
        }
        _ => Json(json!({ "granted": true, "due": false, "run_id": null, "work": null })),
    }
}

async fn reading(State(told): State<Shared>, Path(_run): Path<String>, Json(r): Json<Value>) -> Json<Value> {
    let outcome = match r["outcome"].as_str() {
        Some("changed") => "read",
        Some(other) => other,
        None => "unknown",
    }
    .to_owned();
    told.lock().unwrap().readings.push(r);
    Json(json!({ "outcome": outcome }))
}

async fn result(State(told): State<Shared>, Path(_run): Path<String>, Json(r): Json<Value>) -> Json<Value> {
    told.lock().unwrap().results.push(r);
    Json(json!({}))
}

async fn heartbeat() -> Json<Value> {
    Json(json!({ "keep_going": true }))
}

/// Pages as a browser would render them.
struct Pages;

const PRICING: &str = "Model | Input | Output\ngpt-x | $2.00 / 1M | $10.00 / 1M\n";

impl Render for Pages {
    fn render<'a>(&'a self, url: &'a str) -> BoxFuture<'a, anyhow::Result<Rendered>> {
        Box::pin(async move {
            match url {
                "https://provider.example/pricing" | "https://provider.example/models" => Ok(Rendered {
                    status: Some(200),
                    final_url: url.to_owned(),
                    title: "Pricing".into(),
                    text: PRICING.into(),
                    ..Default::default()
                }),
                "https://provider.example/gone" => {
                    Ok(Rendered { status: Some(404), text: "Not found".into(), ..Default::default() })
                }
                _ => anyhow::bail!("the browser could not open {url}"),
            }
        })
    }

    fn status(&self) -> BoxFuture<'_, BrowserStatus> {
        Box::pin(async { BrowserStatus { ok: true, product: "Chrome/test".into(), message: String::new() } })
    }
}

async fn serve(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    format!("http://{addr}")
}

fn task(page: &str, url: &str, kind: &str, previous_hash: &str, force: bool) -> Value {
    json!({ "page_id": page, "provider_id": "prv_1", "provider": "openai", "url": url, "kind": kind,
            "format": "pacod", "previous_hash": previous_hash, "force": force, "known_models": ["gpt-x"] })
}

#[tokio::test]
async fn a_check_reports_every_page_and_the_run() {
    let feed = serve(Router::new().route(
        "/prices.json",
        get(|| async { r#"{"currency":"USD","models":{"gpt-x":{"input":2.0,"output":10.0},"other":{"input":1}}}"# }),
    ))
    .await;
    let told: Shared = Arc::default();
    let known = fingerprint(PRICING, &["gpt-x".to_owned()]);
    told.lock().unwrap().work = Some(json!({ "extraction": true, "pages": [
        task("ppg_feed", &format!("{feed}/prices.json"), "feed", "", false),
        task("ppg_same", "https://provider.example/pricing", "page", &known, false),
        task("ppg_forced", "https://provider.example/models", "page", &known, true),
        task("ppg_gone", "https://provider.example/gone", "page", "", false),
        task("ppg_broken", "https://provider.example/broken", "page", "", false),
    ] }));
    let core = serve(
        Router::new()
            .route("/api/v1/internal/jobs/lease", post(lease))
            .route("/api/v1/internal/jobs/{run}/readings", post(reading))
            .route("/api/v1/internal/jobs/{run}/heartbeat", post(heartbeat))
            .route("/api/v1/internal/jobs/{run}/result", post(result))
            .with_state(told.clone()),
    )
    .await;
    let policy = Arc::new(EgressPolicy::parse(&["127.0.0.1".to_owned()]).unwrap());
    let runner =
        Arc::new(Runner::new(Core::new(&core, TOKEN).unwrap(), Fetcher::new(policy, "test").unwrap(), Arc::new(Pages)));

    assert!(runner.tick(JOB_PRICE_CHECK).await.unwrap(), "a run happened");
    assert!(!runner.tick(JOB_PRICE_SCAN).await.unwrap(), "nothing to scan");
    let t = told.lock().unwrap();
    // The scraper says who it is and that its browser answers.
    assert_eq!(t.leases[0]["agent"]["service"], "gw-scraper");
    assert_eq!(t.leases[0]["agent"]["browser"]["ok"], true);
    assert_eq!(t.leases[0]["holder"], runner.holder.as_str());

    let by = |page: &str| t.readings.iter().find(|r| r["page_id"] == page).cloned().unwrap();
    let f = by("ppg_feed");
    assert_eq!((f["outcome"].as_str(), f["http_status"].as_u64()), (Some("changed"), Some(200)), "{f}");
    let models = f["extraction"]["models"].as_array().unwrap();
    assert_eq!(models.len(), 1, "only the provider's models: {f}");
    assert_eq!(models[0]["price"]["input"], 2.0);
    assert!(f["text"].is_null(), "a feed sends its models, not its text");
    // An unchanged page is not sent to be read; one to read anyway is, with its text.
    let same = by("ppg_same");
    assert_eq!((same["outcome"].as_str(), same["text"].is_null()), (Some("unchanged"), true), "{same}");
    let forced = by("ppg_forced");
    assert_eq!((forced["outcome"].as_str(), forced["text"].as_str()), (Some("changed"), Some(PRICING)));
    assert_eq!(forced["content_hash"], known.as_str());
    // A page that answers an error, and one the browser cannot open, fail with the reason.
    let gone = by("ppg_gone");
    assert_eq!((gone["outcome"].as_str(), gone["message"].as_str()), (Some("failed"), Some("the page answered 404")));
    assert!(by("ppg_broken")["message"].as_str().unwrap().contains("could not open"));
    // The run ends with what came of each page.
    assert_eq!(t.results.len(), 1);
    let r = &t.results[0];
    assert_eq!(
        (r["run_id"].as_str(), r["job"].as_str(), r["ok"].as_bool()),
        (Some("run_1"), Some(JOB_PRICE_CHECK), Some(true))
    );
    assert_eq!(r["result"]["scraper"]["outcomes"], json!({ "failed": 2, "read": 2, "unchanged": 1 }));
}

#[tokio::test]
async fn a_core_that_cannot_be_reached_is_an_error_not_a_silent_round() {
    let runner = Runner::new(
        Core::new("http://127.0.0.1:9", TOKEN).unwrap(),
        Fetcher::new(Arc::new(EgressPolicy::default()), "test").unwrap(),
        Arc::new(Pages),
    );
    assert!(Arc::new(runner).tick(JOB_PRICE_CHECK).await.is_err());
}
