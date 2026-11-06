//! Prices from the core's side, apart from the sync (`tests/catalog.rs`): imports, what earlier
//! releases left behind, and the internal job API.

mod common;

use common::{Fixture, IngestSink, TestGw};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

const TOKEN: &str = "test-internal-token-0123456789";

async fn setup() -> (TestGw, MockProvider, IngestSink, Fixture) {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    (gw, mock, sink, fx)
}

/// A call on the internal API, as the analytics service makes it.
async fn internal(gw: &TestGw, token: &str, path: &str, body: Value) -> (u16, Value) {
    let r = gw
        .http
        .post(format!("{}/api/v1/internal/jobs{path}", gw.admin))
        .header("x-gw-internal-token", token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

fn rates() -> Value {
    json!({ "currency": "USD", "input": 2.0, "output": 10.0 })
}

#[tokio::test]
async fn an_import_applies_prices_and_leaves_a_pinned_one_alone() {
    let (gw, _mock, _sink, fx) = setup().await;
    let pinned = gw.priced_model(&fx, "openai/pinned", "pinned", json!({}), rates()).await;
    let steady = gw.draft_model(&fx.provider, "openai/steady", "steady", json!({})).await;
    let moved = gw.draft_model(&fx.provider, "openai/moved", "moved", json!({})).await;
    let fresh = gw.draft_model(&fx.provider, "openai/fresh", "fresh", json!({})).await;
    let (st, v) = gw
        .api(
            "POST",
            "/prices/import",
            Some(json!({ "currency": "USD", "prices": [
            { "model": "openai/steady", "input": 2.0, "output": 10.0 },
            { "model": "moved", "input": 2.0, "output": 10.0 }
        ] })),
        )
        .await;
    assert_eq!((st, v["counts"].clone()), (200, json!({ "applied": 2 })), "{v}");
    gw.activate(&steady).await;
    gw.activate(&moved).await;

    // A pin holds the price in effect; it makes no new version.
    let (st, v) = gw.api("POST", &format!("/models/{pinned}/price/pin"), Some(json!({ "pinned": true }))).await;
    assert_eq!((st, v["pinned"].as_bool(), v["version"].as_u64()), (200, Some(true), Some(1)), "{v}");
    let (st, v) = gw.api("POST", &format!("/models/{fresh}/price/pin"), Some(json!({ "pinned": true }))).await;
    assert_eq!(st, 409, "nothing in effect to pin: {v}");
    let (_, v) = gw
        .api(
            "POST",
            "/prices/import",
            Some(json!({ "currency": "USD", "prices": [{ "model": "openai/pinned", "input": 9.0, "output": 18.0 }] })),
        )
        .await;
    assert_eq!(v["counts"], json!({ "pinned": 1 }), "{v}");
}

#[tokio::test]
async fn the_internal_token_opens_the_job_api_and_nothing_else() {
    let (gw, _mock, _sink, _fx) = setup().await;
    let lease = |job: &str| json!({ "job": job, "holder": "analytics-1", "ttl_seconds": 120 });
    assert_eq!(internal(&gw, "wrong-token", "/lease", lease("rollups")).await.0, 401);
    for (method, path) in [("GET", "/providers"), ("GET", "/users"), ("PUT", "/settings")] {
        let st = gw
            .http
            .request(method.parse().unwrap(), format!("{}/api/v1{path}", gw.admin))
            .bearer_auth(TOKEN)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status();
        assert_eq!(st, 401, "{method} {path}");
    }
    // An analytics service of an earlier release is given no price work any more, and a
    // scraper of one none of its jobs.
    for job in ["prices", "price_check", "price_scan"] {
        let (_, l) = internal(&gw, TOKEN, "/lease", lease(job)).await;
        assert_eq!(l["granted"], false, "{job}: {l}");
    }
}

#[tokio::test]
async fn an_extraction_template_elsewhere_moves_to_system_once() {
    let (gw, _mock, _sink, fx) = setup().await;
    gw.priced_model(&fx, "openai/extractor", "extractor", json!({}), rates()).await;
    gw.template(&fx, "price-reader", json!({ "model": "openai/extractor" })).await;
    // A job written by the previous release, which let any workspace hold the template.
    let pool = sqlx::PgPool::connect(&gw.database_url).await.unwrap();
    let doc = json!({ "enabled": false, "schedule": "0 3 * * *", "time_zone": "UTC", "auto_apply_bound": 0.3,
                      "stale_days": 30, "sources": [], "extraction": { "workspace_id": fx.ws, "template": "price-reader" } });
    sqlx::query("INSERT INTO config_objects (id, kind, doc) VALUES ('price_job', 'price_job', $1)")
        .bind(&doc)
        .execute(&pool)
        .await
        .unwrap();

    // The next start of a management instance moves it.
    let again = TestGw::start_on(&gw.database_url, None, |_| {}).await;
    let (_, job) = again.api("GET", "/price-job", None).await;
    assert_eq!(job["extraction"]["workspace_id"], "ws_system", "{job}");
    let (_, list) = again.api("GET", "/orgs/org_system/workspaces/ws_system/templates", None).await;
    assert_eq!(list["items"][0]["name"], "price-reader");
    // The built-in organisation and workspace cannot be deleted.
    assert_eq!(again.api("DELETE", "/orgs/org_system/workspaces/ws_system", None).await.0, 409);
    assert_eq!(again.api("DELETE", "/orgs/org_system", None).await.0, 409);
}

#[tokio::test]
async fn the_retention_job_runs_daily_once_a_retention_is_set() {
    let (gw, _mock, _sink, fx) = setup().await;
    let lease = || json!({ "job": "retention", "holder": "analytics-1", "ttl_seconds": 120 });
    // Nothing is kept for a limited time: nothing to do.
    let (_, l) = internal(&gw, TOKEN, "/lease", lease()).await;
    assert_eq!((l["granted"].as_bool(), l["due"].as_bool()), (Some(true), Some(false)), "{l}");

    // The installation keeps 90 days; one workspace 7 days, through its organisation.
    let (_, mut settings) = gw.api("GET", "/settings", None).await;
    settings["usage_retention_days"] = json!(90);
    assert_eq!(gw.api("PUT", "/settings", Some(settings)).await.0, 200);
    let org_path = format!("/orgs/{}", fx.org);
    let (_, mut org) = gw.api("GET", &org_path, None).await;
    org["settings"]["usage_retention_days"] = json!(7);
    assert_eq!(gw.api("PUT", &org_path, Some(org)).await.0, 200);

    let (_, l) = internal(&gw, TOKEN, "/lease", lease()).await;
    assert_eq!(l["due"], true, "{l}");
    assert_eq!(l["work"]["global_days"], 90);
    let entries = l["work"]["workspaces"].as_array().unwrap();
    assert!(entries.iter().any(|w| w["workspace_id"] == fx.ws && w["days"] == 7), "{l}");
    assert!(entries.iter().all(|w| w["days"] != 90), "workspaces that follow the installation are not listed");
    let run_id = l["run_id"].as_str().unwrap().to_owned();
    let done = json!({ "run_id": run_id, "job": "retention", "finished_at": "2026-10-10T12:00:00Z", "ok": true,
                       "message": "", "result": { "deletions": [{ "days": 7, "deleted": 3 }] } });
    assert_eq!(internal(&gw, TOKEN, &format!("/{run_id}/result"), done).await.0, 200);
    // Done for today.
    let (_, l) = internal(&gw, TOKEN, "/lease", lease()).await;
    assert_eq!(l["due"], false, "{l}");
}
