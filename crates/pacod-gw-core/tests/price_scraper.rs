//! The scraper's jobs from the core's side: the leases of the daily check and the weekly scan,
//! the work each run is handed, the heartbeat that keeps a run's lease, runs left open by a
//! scraper that stopped, and what the console is told about the scraper. The test plays the
//! scraper on the internal API.

mod common;

use common::TestGw;
use serde_json::{Value, json};

const TOKEN: &str = "test-internal-token-0123456789";

async fn internal(gw: &TestGw, path: &str, body: Value) -> (u16, Value) {
    let r = gw
        .http
        .post(format!("{}/api/v1/internal/jobs{path}", gw.admin))
        .header("x-gw-internal-token", TOKEN)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

fn agent(browser_ok: bool) -> Value {
    json!({ "service": "gw-scraper", "version": "9.9.9",
            "browser": { "ok": browser_ok, "product": "Chrome/157.0.8103.27",
                         "message": if browser_ok { "" } else { "gw-browser:9222 refused the connection" } } })
}

async fn lease(gw: &TestGw, job: &str, holder: &str) -> Value {
    let (st, v) =
        internal(gw, "/lease", json!({ "job": job, "holder": holder, "ttl_seconds": 120, "agent": agent(true) })).await;
    assert_eq!(st, 200, "{v}");
    v
}

async fn finish(gw: &TestGw, job: &str, run_id: &str) -> (u16, Value) {
    internal(
        gw,
        &format!("/{run_id}/result"),
        json!({ "run_id": run_id, "job": job, "finished_at": "2026-11-05T12:00:00Z", "ok": true, "message": "",
                "result": { "pages_read": 1 } }),
    )
    .await
}

/// Lets a lease run out, as it does when the scraper that holds it stops.
async fn lapse(gw: &TestGw, job: &str) {
    sqlx::query("UPDATE jobs SET lease_until = now() - interval '1 minute' WHERE name = $1")
        .bind(job)
        .execute(gw.app.store.pool())
        .await
        .unwrap();
}

fn urls(work: &Value) -> Vec<String> {
    work["pages"].as_array().unwrap().iter().map(|p| p["url"].as_str().unwrap().to_owned()).collect()
}

#[tokio::test]
async fn the_check_reads_the_confirmed_pages_on_its_own_schedule() {
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("openai", "https://api.openai.com/v1").await;
    gw.draft_model(&fx.provider, "openai/gpt-x", "gpt-x", json!({})).await;

    // Before any scraper asked, the console is told that none runs.
    let (_, v) = gw.api("PUT", "/price-job", Some(json!({ "enabled": true }))).await;
    assert_eq!(v["scraper"]["present"], false, "{v}");
    let problems = v["problems"].to_string();
    assert!(problems.contains("gw-scraper") && problems.contains("No page is confirmed"), "{problems}");

    // Nothing to check yet: the run is skipped, and the schedule moves on.
    let l = lease(&gw, "price_check", "scraper-1").await;
    assert_eq!((l["granted"].as_bool(), l["due"].as_bool()), (Some(true), Some(false)), "{l}");
    let (_, v) = gw.api("GET", "/price-job", None).await;
    assert!(v["jobs"]["check"]["last_started_at"].is_string() && v["jobs"]["check"]["next_run_at"].is_string(), "{v}");
    assert_eq!((v["scraper"]["present"].as_bool(), v["scraper"]["version"].as_str()), (Some(true), Some("9.9.9")));
    assert!(!v["problems"].to_string().contains("gw-scraper"), "{v}");

    // A page is added: it is checked at the next run, asked for alone.
    let page = gw
        .create("/price-pages", json!({ "provider_id": fx.provider, "url": "https://openai.example/pricing" }))
        .await
        .id;
    let gone =
        gw.create("/price-pages", json!({ "provider_id": fx.provider, "url": "https://openai.example/old" })).await.id;
    assert_eq!(gw.api("POST", &format!("/price-pages/{gone}/dismiss"), None).await.0, 200);
    let l = lease(&gw, "price_check", "scraper-1").await;
    assert_eq!(l["due"], true, "{l}");
    let run_id = l["run_id"].as_str().unwrap().to_owned();
    assert_eq!(urls(&l["work"]), ["https://openai.example/pricing"], "a dismissed page is not read: {l}");
    let task = &l["work"]["pages"][0];
    assert_eq!(
        (task["page_id"].as_str(), task["kind"].as_str(), task["force"].as_bool()),
        (Some(page.as_str()), Some("page"), Some(true))
    );
    assert_eq!(task["provider"].as_str(), Some(fx.provider_name.as_str()));
    assert!(task["known_models"].as_array().unwrap().contains(&json!("gpt-x")), "{task}");
    assert_eq!(l["work"]["extraction"], false);
    assert!(gw.api("GET", "/price-job", None).await.1["problems"].to_string().contains("No extraction model"));
    // Another scraper gets nothing while the run is on.
    assert_eq!(lease(&gw, "price_check", "scraper-2").await["granted"], false);
    // The run ends with what the scraper reports.
    let (st, v) = finish(&gw, "price_check", &run_id).await;
    assert_eq!(st, 200, "{v}");
    let (_, runs) = gw.api("GET", "/price-job/runs?job=check", None).await;
    assert_eq!(
        (
            runs[0]["id"].as_str(),
            runs[0]["trigger"].as_str(),
            runs[0]["models"].clone(),
            runs[0]["result"]["pages_read"].as_u64()
        ),
        (Some(run_id.as_str()), Some("page"), json!([page]), Some(1))
    );
    assert_eq!(gw.api("GET", "/price-job/runs?job=nonsense", None).await.0, 422);

    // "Run now" checks every page, by hand.
    assert_eq!(gw.api("POST", "/price-job/run", None).await.0, 202);
    let l = lease(&gw, "price_check", "scraper-1").await;
    let run_id = l["run_id"].as_str().unwrap().to_owned();
    finish(&gw, "price_check", &run_id).await;
    let (_, runs) = gw.api("GET", "/price-job/runs?job=check", None).await;
    assert_eq!(runs[0]["trigger"], "manual");
    // Done until the schedule says so.
    assert_eq!(lease(&gw, "price_check", "scraper-1").await["due"], false);

    // Activating a model asks for the pages of its provider, read even when unchanged.
    let model = gw
        .priced_model(
            &fx,
            "openai/gpt-y",
            "gpt-y",
            json!({}),
            json!({ "currency": "USD", "input": 1.0, "output": 2.0 }),
        )
        .await;
    let l = lease(&gw, "price_check", "scraper-1").await;
    assert_eq!((l["due"].as_bool(), l["work"]["pages"][0]["force"].as_bool()), (Some(true), Some(true)), "{l}");
    let run_id = l["run_id"].as_str().unwrap().to_owned();
    finish(&gw, "price_check", &run_id).await;
    let (_, runs) = gw.api("GET", "/price-job/runs?job=check", None).await;
    assert_eq!(runs[0]["trigger"], "activation");
    let _ = model;
}

#[tokio::test]
async fn the_scan_starts_from_the_pages_and_the_presets_pages() {
    let gw = TestGw::start(None).await;
    // DeepSeek at its own address: its preset names its pricing page. A provider elsewhere
    // with no pages has nowhere to start.
    let deepseek = gw
        .create("/providers", json!({ "name": "deepseek", "family": "openai", "base_url": "https://api.deepseek.com" }))
        .await
        .id;
    let local = gw
        .create("/providers", json!({ "name": "local", "family": "openai", "base_url": "https://llm.example.com/v1" }))
        .await
        .id;
    gw.create("/price-pages", json!({ "provider_id": local, "url": "https://llm.example.com/models" })).await;
    gw.create(
        "/price-pages",
        json!({ "provider_id": local, "url": "https://llm.example.com/prices.json", "kind": "feed" }),
    )
    .await;
    assert_eq!(gw.api("PUT", "/price-job", Some(json!({ "enabled": true }))).await.0, 200);

    // Never scanned: due at once.
    let l = lease(&gw, "price_scan", "scraper-1").await;
    assert_eq!(l["due"], true, "{l}");
    let providers = l["work"]["providers"].as_array().unwrap();
    let of = |id: &str| providers.iter().find(|p| p["provider_id"] == id).cloned().unwrap();
    assert_eq!(
        of(&deepseek)["roots"],
        json!([{ "url": "https://api-docs.deepseek.com/quick_start/pricing", "page_id": null, "seed": true }])
    );
    let roots = of(&local)["roots"].clone();
    assert_eq!(roots.as_array().unwrap().len(), 1, "a feed is no place to look for pages: {roots}");
    assert_eq!(of(&local)["known_urls"].as_array().unwrap().len(), 2);
    assert_eq!(
        (l["work"]["max_pages_per_provider"].as_u64(), l["work"]["keywords"][0].as_str()),
        (Some(12), Some("pricing"))
    );
    let run_id = l["run_id"].as_str().unwrap().to_owned();
    finish(&gw, "price_scan", &run_id).await;
    let (_, runs) = gw.api("GET", "/price-job/runs?job=scan", None).await;
    assert_eq!((runs[0]["trigger"].as_str(), runs[0]["models"].as_array().map(Vec::len)), (Some("schedule"), Some(2)));

    // Asked for by hand, the scan runs again; the check is a job of its own.
    assert_eq!(gw.api("POST", "/price-job/scan", None).await.0, 202);
    let (_, v) = gw.api("GET", "/price-job", None).await;
    assert_eq!(v["jobs"]["scan"]["run_requested"], true, "{v}");
    assert_eq!(v["jobs"]["scan"]["schedule"], "41 4 * * 1");
    let l = lease(&gw, "price_scan", "scraper-1").await;
    assert_eq!(l["due"], true);
}

#[tokio::test]
async fn a_run_left_open_is_closed_and_run_once_more() {
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("openai", "https://api.openai.com/v1").await;
    gw.create("/price-pages", json!({ "provider_id": fx.provider, "url": "https://openai.example/pricing" })).await;
    assert_eq!(gw.api("PUT", "/price-job", Some(json!({ "enabled": true }))).await.0, 200);
    let first = lease(&gw, "price_check", "scraper-1").await;
    let run_id = first["run_id"].as_str().unwrap().to_owned();

    // The heartbeat keeps the run its holder's; another holder is told to stop.
    let beat = |holder: &str| json!({ "holder": holder, "ttl_seconds": 120 });
    let (st, v) = internal(&gw, &format!("/{run_id}/heartbeat"), beat("scraper-1")).await;
    assert_eq!((st, v["keep_going"].as_bool()), (200, Some(true)), "{v}");
    assert_eq!(internal(&gw, &format!("/{run_id}/heartbeat"), beat("scraper-2")).await.1["keep_going"], false);

    // The scraper stops; another one takes over the lease. The run is closed as failed and
    // run once more, over the same pages.
    lapse(&gw, "price_check").await;
    let second = lease(&gw, "price_check", "scraper-2").await;
    assert_eq!(second["due"], true, "{second}");
    let retry = second["run_id"].as_str().unwrap().to_owned();
    assert_ne!(retry, run_id);
    assert_eq!(urls(&second["work"]), urls(&first["work"]));
    let (_, runs) = gw.api("GET", "/price-job/runs?job=check", None).await;
    assert_eq!((runs[0]["trigger"].as_str(), runs[0]["ok"].clone()), (Some("retry"), Value::Null));
    assert_eq!(runs[1]["ok"], false);
    assert!(runs[1]["message"].as_str().unwrap().contains("stopped"), "{runs}");
    // The first scraper, back, is told that the run is no longer its own.
    assert_eq!(internal(&gw, &format!("/{run_id}/heartbeat"), beat("scraper-1")).await.1["keep_going"], false);
    assert_eq!(finish(&gw, "price_check", &run_id).await.0, 409);

    // The second try is left open too: closed, and not tried a third time.
    lapse(&gw, "price_check").await;
    let third = lease(&gw, "price_check", "scraper-3").await;
    assert_eq!((third["granted"].as_bool(), third["due"].as_bool()), (Some(true), Some(false)), "{third}");
    let (_, v) = gw.api("GET", "/price-job", None).await;
    assert_eq!(v["jobs"]["check"]["running"], false);
    assert_eq!(v["jobs"]["check"]["last_run"]["trigger"], "retry");
}

#[tokio::test]
async fn a_scraper_without_its_browser_is_named_on_the_console() {
    let gw = TestGw::start(None).await;
    assert_eq!(gw.api("PUT", "/price-job", Some(json!({ "enabled": true }))).await.0, 200);
    let (st, _) = internal(
        &gw,
        "/lease",
        json!({ "job": "price_scan", "holder": "s", "ttl_seconds": 60, "agent": agent(false) }),
    )
    .await;
    assert_eq!(st, 200);
    let (_, v) = gw.api("GET", "/price-job", None).await;
    assert_eq!(v["scraper"]["browser"]["ok"], false, "{v}");
    assert!(v["problems"].to_string().contains("gw-browser:9222 refused the connection"), "{v}");
    // A stored schedule an earlier release accepted and this one cannot read is named too.
    sqlx::query(
        "UPDATE config_objects SET doc = jsonb_set(doc, '{scan_schedule}', '\"0 3 1 * 1\"') WHERE kind = 'price_job'",
    )
    .execute(gw.app.store.pool())
    .await
    .unwrap();
    gw.app.gw.config.reload_all().await.unwrap();
    let (_, v) = gw.api("GET", "/price-job", None).await;
    assert!(v["problems"].to_string().contains("scan_schedule"), "{v}");
}
