//! The price tracker's pages: what an administrator adds, confirms and dismisses, the sources of
//! earlier releases read into pages, and the model that reads them.

mod common;

use common::TestGw;
use serde_json::{Value, json};

fn rates() -> Value {
    json!({ "currency": "USD", "input": 2.0, "output": 10.0 })
}

/// The pages the API lists, as (url, status, found_by).
async fn pages(gw: &TestGw) -> Vec<(String, String, String)> {
    let (st, v) = gw.api("GET", "/price-pages", None).await;
    assert_eq!(st, 200, "{v}");
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let s = |k: &str| p[k].as_str().unwrap_or_default().to_owned();
            (s("url"), s("status"), s("found_by"))
        })
        .collect()
}

#[tokio::test]
async fn pages_are_added_confirmed_dismissed_and_checked() {
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("openai", "https://api.openai.com/v1").await;

    let (st, page) = gw
        .api(
            "POST",
            "/price-pages",
            Some(json!({ "provider_id": fx.provider, "url": "https://Developers.OpenAI.com/api/docs/pricing/#text" })),
        )
        .await;
    assert_eq!(st, 201, "{page}");
    assert_eq!(
        (page["url"].as_str(), page["status"].as_str(), page["found_by"].as_str(), page["kind"].as_str()),
        (Some("https://developers.openai.com/api/docs/pricing"), Some("active"), Some("admin"), Some("page")),
        "stored normalised, read from the next check on"
    );
    let id = page["id"].as_str().unwrap().to_owned();
    // An address is one page per provider, however it is written.
    let (st, v) = gw
        .api(
            "POST",
            "/price-pages",
            Some(json!({ "provider_id": fx.provider, "url": "https://developers.openai.com/api/docs/pricing?utm_source=x" })),
        )
        .await;
    assert_eq!(st, 409, "{v}");
    for (bad, why) in [
        (json!({ "provider_id": "prv_nope", "url": "https://example.com/pricing" }), "provider"),
        (json!({ "provider_id": fx.provider, "url": "http://169.254.169.254/latest" }), "link-local"),
        (json!({ "provider_id": fx.provider, "url": "ftp://example.com/pricing" }), "http"),
        (json!({ "provider_id": fx.provider, "url": "https://example.com/p", "key_prefix": "zai" }), "feeds"),
    ] {
        let (st, v) = gw.api("POST", "/price-pages", Some(bad.clone())).await;
        assert_eq!(st, 422, "{bad}: {v}");
        assert!(v["error"]["message"].as_str().unwrap().contains(why), "{why}: {v}");
    }
    // A feed takes its prefix.
    let feed = gw
        .create(
            "/price-pages",
            json!({ "provider_id": fx.provider, "kind": "feed", "format": "litellm", "key_prefix": " openai ",
                    "url": "https://raw.githubusercontent.com/BerriAI/litellm/main/model_prices_and_context_window.json",
                    "priority": 5 }),
        )
        .await;
    assert_eq!(feed.body["key_prefix"], "openai");

    // A new page is asked to be checked; the list shows that with the provider's name.
    let (_, list) = gw.api("GET", &format!("/price-pages?provider_id={}", fx.provider), None).await;
    let first = &list[0];
    assert_eq!(
        (first["id"].as_str(), first["provider"].as_str()),
        (Some(id.as_str()), Some(fx.provider_name.as_str()))
    );
    assert_eq!(first["state"]["check_requested"], true, "{list}");
    assert_eq!(list[1]["priority"], 5, "by priority");

    // Dismissed: not read, and asking for a check is refused.
    let (st, v) = gw.api("POST", &format!("/price-pages/{id}/dismiss"), None).await;
    assert_eq!((st, v["status"].as_str()), (200, Some("dismissed")), "{v}");
    assert!(v["decided_by"].is_string() && v["decided_at"].is_string());
    assert_eq!(gw.api("POST", &format!("/price-pages/{id}/dismiss"), None).await.0, 409);
    assert_eq!(gw.api("POST", &format!("/price-pages/{id}/check"), None).await.0, 409);
    let (_, only) = gw.api("GET", "/price-pages?status=dismissed", None).await;
    assert_eq!(only.as_array().unwrap().len(), 1);
    // Confirmed again: read at the next run, and a check can be asked for.
    let (st, v) = gw.api("POST", &format!("/price-pages/{id}/confirm"), None).await;
    assert_eq!((st, v["status"].as_str()), (200, Some("active")), "{v}");
    assert_eq!(gw.api("POST", &format!("/price-pages/{id}/check"), None).await.0, 202);

    // A provider whose pages are read cannot go; its other pages go with it.
    let other = gw
        .create(
            "/providers",
            json!({ "name": "other", "family": "openai", "base_url": "https://api.other.example/v1" }),
        )
        .await
        .id;
    let page =
        gw.create("/price-pages", json!({ "provider_id": other, "url": "https://other.example/pricing" })).await.id;
    let (st, v) = gw.api("DELETE", &format!("/providers/{other}"), None).await;
    assert_eq!(st, 409, "{v}");
    assert_eq!(gw.api("POST", &format!("/price-pages/{page}/dismiss"), None).await.0, 200);
    assert_eq!(gw.api("DELETE", &format!("/providers/{other}"), None).await.0, 204);
    assert_eq!(gw.api("GET", &format!("/price-pages/{page}"), None).await.0, 404);

    // Pages are part of the configuration export.
    let (_, export) = gw.api("GET", "/system/config-export", None).await;
    let kinds: Vec<&str> = export["objects"].as_array().unwrap().iter().filter_map(|o| o["kind"].as_str()).collect();
    assert!(kinds.contains(&"price_page"), "{kinds:?}");
}

#[tokio::test]
async fn sources_of_an_earlier_release_become_pages_once() {
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("openai", "https://api.openai.com/v1").await;
    // A job written by an earlier release, with two sources: read into pages at the next start.
    let doc = json!({ "enabled": true, "schedule": "0 3 * * *", "time_zone": "UTC", "auto_apply_bound": 0.3, "stale_days": 30,
                      "sources": [
                          { "provider_id": fx.provider, "kind": "feed", "url": "https://prices.example/list.json", "format": "litellm", "key_prefix": "openai" },
                          { "provider_id": fx.provider, "kind": "page", "url": "https://openai.example/pricing/" },
                          { "provider_id": "prv_gone", "kind": "feed", "url": "https://prices.example/list.json" }
                      ] });
    sqlx::query("INSERT INTO config_objects (id, kind, doc) VALUES ('price_job', 'price_job', $1)")
        .bind(&doc)
        .execute(gw.app.store.pool())
        .await
        .unwrap();
    let again = TestGw::start_on(&gw.database_url, None, |_| {}).await;
    assert_eq!(
        pages(&again).await,
        [
            ("https://prices.example/list.json".to_owned(), "active".to_owned(), "migration".to_owned()),
            ("https://openai.example/pricing".to_owned(), "active".to_owned(), "migration".to_owned()),
        ],
        "in their order, without the source whose provider is gone"
    );
    let (_, job) = again.api("GET", "/price-job", None).await;
    assert_eq!(
        (job["pages_migrated"].as_bool(), job["sources"].as_array().map(Vec::len)),
        (Some(true), Some(3)),
        "the sources stay for an instance of an earlier release: {job}"
    );
    let (_, list) = again.api("GET", "/price-pages", None).await;
    assert_eq!((list[0]["priority"].as_u64(), list[0]["key_prefix"].as_str()), (Some(0), Some("openai")));

    // From now on the sources are the earlier release's: saving the job leaves them as they
    // are, and they are no longer checked (one names a provider that is gone).
    let mut body = job.clone();
    body["sources"] = json!([{ "provider_id": "prv_nope", "kind": "feed", "url": "ftp://x.example" }]);
    body["pages_migrated"] = json!(false);
    let (st, v) = again.api("PUT", "/price-job", Some(body)).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!((v["sources"].as_array().map(Vec::len), v["pages_migrated"].as_bool()), (Some(3), Some(true)));
    // A third start finds nothing to do.
    let third = TestGw::start_on(&gw.database_url, None, |_| {}).await;
    assert_eq!(pages(&third).await.len(), 2);
}

#[tokio::test]
async fn a_job_saved_with_sources_makes_them_pages_at_once() {
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("openai", "https://api.openai.com/v1").await;
    let job = json!({ "enabled": true, "sources": [{ "provider_id": fx.provider, "kind": "feed", "url": "https://prices.example/list.json" }] });
    for (bad, why) in [
        (json!({ "sources": [{ "provider_id": "prv_nope", "kind": "feed", "url": "https://x.example" }] }), "provider"),
        (json!({ "sources": [{ "provider_id": fx.provider, "kind": "feed", "url": "ftp://x.example" }] }), "source"),
        (
            json!({ "sources": [{ "provider_id": fx.provider, "kind": "page", "url": "https://x.example" }] }),
            "extraction model",
        ),
        (json!({ "scan_schedule": "weekly" }), "scan_schedule"),
        (json!({ "scan_schedule": "0 3 1 * 1" }), "scan_schedule"),
        (json!({ "max_extractions_per_run": 0 }), "max_extractions_per_run"),
        (json!({ "scan": { "max_pages_per_provider": 500 } }), "scan"),
        (json!({ "extraction_model": "openai/nothing" }), "no model"),
    ] {
        let mut body = job.clone();
        body.as_object_mut().unwrap().extend(bad.as_object().unwrap().clone());
        let (st, v) = gw.api("PUT", "/price-job", Some(body)).await;
        assert_eq!(st, 422, "{bad}: {v}");
        assert!(v["error"]["message"].as_str().unwrap().contains(why), "{why}: {v}");
    }
    let (st, v) = gw.api("PUT", "/price-job", Some(job)).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["pages_migrated"], true);
    assert_eq!(pages(&gw).await.len(), 1);
    // The defaults of the new settings.
    assert_eq!(
        (
            v["scan_schedule"].as_str(),
            v["max_extractions_per_run"].as_u64(),
            v["scan"]["max_pages_per_provider"].as_u64()
        ),
        (Some("41 4 * * 1"), Some(60), Some(12))
    );
}

#[tokio::test]
async fn the_extraction_model_is_an_active_chat_model_with_structured_output() {
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("openai", "https://api.openai.com/v1").await;
    let plain = gw.priced_model(&fx, "openai/plain", "plain", json!({}), rates()).await;
    let reader = gw
        .priced_model(&fx, "openai/reader", "reader", json!({ "capabilities": { "structured_output": true } }), rates())
        .await;
    let draft = gw
        .draft_model(&fx.provider, "openai/draft", "draft", json!({ "capabilities": { "structured_output": true } }))
        .await;
    for (model, why) in [(plain.as_str(), "structured output"), (draft.as_str(), "not active")] {
        let (st, v) = gw.api("PUT", "/price-job", Some(json!({ "extraction_model": model }))).await;
        assert_eq!(st, 422, "{model}: {v}");
        assert!(v["error"]["message"].as_str().unwrap().contains(why), "{why}: {v}");
    }
    // Named by id, stored by name; it replaces the template of earlier releases.
    gw.create(
        "/orgs/org_system/workspaces/ws_system/templates",
        json!({ "name": "price-reader", "spec": { "model": "openai/reader" } }),
    )
    .await;
    let (st, v) = gw
        .api(
            "PUT",
            "/price-job",
            Some(json!({ "extraction_model": reader, "extraction": { "template": "price-reader" } })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!((v["extraction_model"].as_str(), v.get("extraction")), (Some("openai/reader"), None), "{v}");
}
