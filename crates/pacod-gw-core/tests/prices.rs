//! The price tracker from the core's side: what it makes of the prices and properties the pages
//! state, read by the extraction model through the gateway's own data plane, and the imports.
//! The test plays the scraper on the internal API: it reports each page of a check run.

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

/// A call on the internal API, as the scraper makes it.
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

/// Takes the check's lease and answers the run it starts, which must be due.
async fn check_run(gw: &TestGw) -> (String, Value) {
    let (st, l) =
        internal(gw, TOKEN, "/lease", json!({ "job": "price_check", "holder": "scraper-1", "ttl_seconds": 120 })).await;
    assert_eq!((st, l["due"].as_bool()), (200, Some(true)), "{l}");
    (l["run_id"].as_str().unwrap().to_owned(), l["work"].clone())
}

async fn finish(gw: &TestGw, run_id: &str) {
    let done = json!({ "run_id": run_id, "job": "price_check", "finished_at": "2026-11-05T12:00:00Z", "ok": true,
                       "message": "", "result": {} });
    assert_eq!(internal(gw, TOKEN, &format!("/{run_id}/result"), done).await.0, 200);
}

/// What a feed says about one model: its price.
fn priced(model: &str, input: f64, output: f64) -> Value {
    json!({ "model": model, "api_id_stated": true,
            "price": { "model_id": model, "currency": "USD", "input": input, "output": output, "source": "feed",
                       "evidence_url": "", "evidence_text": "input 2.00, output 10.00" } })
}

/// The scraper's account of a feed page, read into `models`.
fn feed_reading(page: &str, hash: &str, models: Vec<Value>) -> Value {
    json!({ "page_id": page, "outcome": "changed", "content_hash": hash, "extraction": { "models": models } })
}

/// The result of one model in a reading's answer.
fn outcome(r: &Value, name: &str) -> Value {
    r["settled"]
        .as_array()
        .unwrap_or_else(|| panic!("nothing settled: {r}"))
        .iter()
        .find(|m| m["model"] == name)
        .cloned()
        .unwrap_or_else(|| panic!("{name} missing in {r}"))
}

fn rates() -> Value {
    json!({ "currency": "USD", "input": 2.0, "output": 10.0 })
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
async fn a_check_applies_every_valid_price_and_leaves_pinned_prices_alone() {
    let (gw, _mock, _sink, fx) = setup().await;
    // Six models: two priced by hand (one of them pinned, one no page names), two by an import,
    // one a draft waiting for a price.
    let manual = gw.priced_model(&fx, "openai/manual", "manual", json!({}), rates()).await;
    let pinned = gw.priced_model(&fx, "openai/pinned", "pinned", json!({}), rates()).await;
    gw.priced_model(&fx, "openai/lonely", "lonely", json!({}), rates()).await;
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
    // An import leaves a pinned price alone too.
    let (_, v) = gw
        .api(
            "POST",
            "/prices/import",
            Some(json!({ "currency": "USD", "prices": [{ "model": "openai/pinned", "input": 9.0, "output": 18.0 }] })),
        )
        .await;
    assert_eq!(v["counts"], json!({ "pinned": 1 }), "{v}");

    // The provider's price list is a page of the tracker's; the job is switched on.
    let page = gw
        .create(
            "/price-pages",
            json!({ "provider_id": fx.provider, "kind": "feed", "url": "https://prices.example/list.json" }),
        )
        .await
        .id;
    let (st, v) = gw.api("PUT", "/price-job", Some(json!({ "enabled": true, "time_zone": "Europe/Vienna" }))).await;
    assert_eq!(st, 200, "{v}");
    // The internal API is for the scraper only, and its token opens nothing else.
    assert_eq!(
        internal(&gw, "wrong-token", "/lease", json!({ "job": "price_check", "holder": "x", "ttl_seconds": 60 }))
            .await
            .0,
        401
    );
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
    // An analytics service of an earlier release is given no price work any more.
    let (_, l) =
        internal(&gw, TOKEN, "/lease", json!({ "job": "prices", "holder": "analytics-1", "ttl_seconds": 120 })).await;
    assert_eq!(l["granted"], false, "{l}");

    // A proposal an earlier release left waiting is settled by the price that applies now.
    legacy_proposal(&gw, &fresh, 1, 0.5, 2.0).await;

    // The page is new: the check reads it.
    let (run_id, work) = check_run(&gw).await;
    assert_eq!(work["pages"][0]["page_id"], page.as_str());
    let reading = feed_reading(
        &page,
        "h1",
        vec![
            priced("manual", 3.0, 15.0), // entered by hand: replaced
            priced("pinned", 3.0, 15.0), // pinned: left alone
            priced("steady", 2.2, 10.0), // +10 %
            priced("moved", 4.0, 10.0),  // +100 %: applied all the same
            priced("fresh", 1.0, 4.0),   // a first price
            priced("nothing", 1.0, 4.0), // not a model of the repository
            json!({ "model": "STEADY", "price": { "model_id": "steady", "currency": "USD", "input": 9.0, "output": 1.0,
                    "source": "feed", "evidence_url": "", "evidence_text": "" } }),
        ],
    );
    let (st, r) = internal(&gw, TOKEN, &format!("/{run_id}/readings"), reading.clone()).await;
    assert_eq!((st, r["outcome"].as_str()), (200, Some("read")), "{r}");
    assert_eq!(
        (outcome(&r, "openai/manual")["outcome"].as_str(), outcome(&r, "openai/manual")["version"].as_u64()),
        (Some("applied"), Some(2))
    );
    assert_eq!(outcome(&r, "openai/pinned")["outcome"], "pinned");
    let s = outcome(&r, "openai/steady");
    assert_eq!(
        (s["outcome"].as_str(), s["version"].as_u64(), s["page"].as_str()),
        (Some("applied"), Some(2), Some("https://prices.example/list.json"))
    );
    assert!((s["change"].as_f64().unwrap() - 0.1).abs() < 1e-9, "the first naming of a model counts: {s}");
    assert_eq!(
        (outcome(&r, "openai/moved")["outcome"].as_str(), outcome(&r, "openai/moved")["detail"].as_str()),
        (Some("applied"), Some("a change of 100 %"))
    );
    assert_eq!(
        (outcome(&r, "openai/fresh")["outcome"].as_str(), outcome(&r, "openai/fresh")["detail"].as_str()),
        (Some("applied"), Some("the model's first price"))
    );
    assert_eq!(outcome(&r, "openai/lonely")["outcome"], "no_price");
    // A reading for a page the run does not check is refused.
    assert_eq!(
        internal(&gw, TOKEN, &format!("/{run_id}/readings"), feed_reading("ppg_other", "h", vec![])).await.0,
        409
    );
    finish(&gw, &run_id).await;
    assert_eq!(internal(&gw, TOKEN, &format!("/{run_id}/readings"), reading.clone()).await.0, 409, "the run is over");

    // The prices in effect changed, with their source and evidence.
    let (_, current) = gw.api("GET", "/prices", None).await;
    let price = |name: &str| current.as_array().unwrap().iter().find(|p| p["model"] == name).cloned().unwrap();
    let steady_now = price("openai/steady");
    assert_eq!(
        (steady_now["input"].as_f64(), steady_now["source"].as_str(), steady_now["version"].as_u64()),
        (Some(2.2), Some("feed"), Some(2))
    );
    assert_eq!(steady_now["evidence_url"], "https://prices.example/list.json");
    assert_eq!(price("openai/moved")["input"], 4.0);
    assert_eq!(
        (price("openai/manual")["input"].as_f64(), price("openai/manual")["source"].as_str()),
        (Some(3.0), Some("feed"))
    );
    assert_eq!(
        (price("openai/pinned")["input"].as_f64(), price("openai/pinned")["pinned"].as_bool()),
        (Some(2.0), Some(true))
    );
    assert_eq!(price("openai/fresh")["input"], 1.0);
    let (_, versions) = gw.api("GET", &format!("/models/{fresh}/prices"), None).await;
    let left = versions.as_array().unwrap().iter().find(|p| p["evidence_url"] == "" && p["version"] == 1).cloned();
    assert_eq!(left.map(|p| p["status"].clone()), Some(json!("superseded")), "{versions}");

    // The run is on record with the page and what came of each model.
    let (_, runs) = gw.api("GET", "/price-job/runs?job=check", None).await;
    let last = &runs[0];
    assert_eq!(
        (last["id"].as_str(), last["trigger"].as_str(), last["ok"].as_bool()),
        (Some(run_id.as_str()), Some("schedule"), Some(true))
    );
    assert_eq!(last["result"]["pages"][0]["outcome"], "read");
    assert!(last["result"]["models"].as_array().unwrap().iter().any(|m| m["model"] == "openai/steady"), "{last}");
    let (_, page_now) = gw.api("GET", "/price-pages", None).await;
    assert_eq!(
        (page_now[0]["state"]["outcome"].as_str(), page_now[0]["state"]["check_requested"].as_bool()),
        (Some("read"), Some(false))
    );

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
    assert!(gw.api("GET", "/price-proposals", None).await.1.as_array().unwrap().is_empty());

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
        ["openai/lonely", "openai/manual", "openai/moved", "openai/steady"],
        "a pinned price is not the tracker's to check, a draft not called: {alerts}"
    );

    // The next check finds the feed unchanged. What the page said still stands: the prices it
    // names are confirmed and no longer stale; the one no page names stays stale.
    assert_eq!(gw.api("POST", "/price-job/run", None).await.0, 202);
    let (run_id, _) = check_run(&gw).await;
    let mut again = reading;
    again["extraction"]["models"] = json!([]);
    let (_, r) = internal(&gw, TOKEN, &format!("/{run_id}/readings"), again).await;
    assert_eq!(r["outcome"], "unchanged", "{r}");
    assert_eq!(outcome(&r, "openai/steady")["outcome"], "unchanged");
    finish(&gw, &run_id).await;
    let (_, alerts) = gw.api("GET", "/monitoring/alerts", None).await;
    assert_eq!(stale(&alerts), ["openai/lonely"], "{alerts}");
    let (_, versions) = gw.api("GET", &format!("/models/{steady}/prices"), None).await;
    let active = versions.as_array().unwrap().iter().find(|p| p["in_effect"] == true).unwrap();
    assert!(active["confirmed_at"].is_string(), "{active}");
    assert_eq!(
        (active["version"].as_u64(), active["input"].as_f64()),
        (Some(2), Some(2.2)),
        "a confirmation is not a new version"
    );
}

/// What the mock answers for a page, as the extraction model would.
fn reply(models: Value) -> String {
    use base64::Engine as _;
    let text = json!({ "models": models }).to_string();
    format!("reply={}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text))
}

fn script(mock: &MockProvider, replies: Vec<String>) {
    mock.set_script(pacod_gw_testkit::mock::Behavior::parse_all(replies).unwrap());
}

#[tokio::test]
async fn a_page_is_read_by_the_extraction_model_and_its_properties_applied() {
    let (gw, mock, sink, fx) = setup().await;
    let reader = json!({ "capabilities": { "structured_output": true }, "context_window": 32000 });
    gw.priced_model(&fx, "openai/extractor", "extractor", reader, rates()).await;
    let target = gw.priced_model(&fx, "openai/target", "target", json!({ "context_window": 128000 }), rates()).await;
    let page = gw
        .create("/price-pages", json!({ "provider_id": fx.provider, "url": "https://provider.example/models" }))
        .await
        .id;
    assert_eq!(gw.api("PUT", "/price-job", Some(json!({ "enabled": true }))).await.0, 200);
    let page_text = "Models\nModel | Context | Input | Output\ntarget | 400K | $2.00 / 1M | $10.00 / 1M\n";
    let changed = |hash: &str| {
        json!({ "page_id": page, "outcome": "changed", "content_hash": hash, "text_hash": hash,
                                       "http_status": 200, "text": page_text })
    };

    // Without an extraction model the page is checked, not read, and stays to be read.
    let (run_id, work) = check_run(&gw).await;
    assert_eq!(work["extraction"], false);
    let (_, r) = internal(&gw, TOKEN, &format!("/{run_id}/readings"), changed("h1")).await;
    assert_eq!(
        (r["outcome"].as_str(), r["message"].as_str()),
        (Some("not_read"), Some("no extraction model is set")),
        "{r}"
    );
    finish(&gw, &run_id).await;

    assert_eq!(
        gw.api("PUT", "/price-job", Some(json!({ "enabled": true, "extraction_model": "openai/extractor" }))).await.0,
        200
    );
    let target_page = |context: u64, input: f64| {
        json!([{ "model": "target", "display_name": "Target", "api_id_stated": true, "currency": "USD",
                 "input": input, "output": 10.0, "context_window": context, "input_modalities": ["text", "image"],
                 "vision": true, "evidence": "$2.00 / 1M" },
               { "model": "target-mini", "api_id_stated": true, "input": 0.5, "output": 2.0 }])
    };
    script(&mock, vec![reply(target_page(400_000, 2.5))]);
    assert_eq!(gw.api("POST", "/price-job/run", None).await.0, 202);
    let (run_id, work) = check_run(&gw).await;
    assert_eq!(
        (work["extraction"].as_bool(), work["pages"][0]["force"].as_bool()),
        (Some(true), Some(true)),
        "never read yet"
    );
    let (st, r) = internal(&gw, TOKEN, &format!("/{run_id}/readings"), changed("h1")).await;
    assert_eq!((st, r["outcome"].as_str()), (200, Some("read")), "{r}");
    let t = outcome(&r, "openai/target");
    assert_eq!(
        (t["outcome"].as_str(), t["page"].as_str()),
        (Some("applied"), Some("https://provider.example/models")),
        "{t}"
    );
    let mut fields: Vec<&str> = t["properties"].as_array().unwrap().iter().map(|f| f.as_str().unwrap()).collect();
    fields.sort_unstable();
    assert_eq!(fields, ["capabilities.vision", "context_window", "modalities.input"], "{t}");
    finish(&gw, &run_id).await;

    // The call went through the gateway as the built-in workspace, to the extraction model.
    let sent = mock.captured().last().unwrap().json().unwrap();
    assert_eq!(
        (sent["model"].as_str(), sent["response_format"]["type"].as_str()),
        (Some("extractor"), Some("json_schema"))
    );
    assert_eq!(sent["response_format"]["json_schema"]["name"], "price_page", "{sent}");
    assert!(sent["messages"][0]["content"].as_str().unwrap().contains("target | 400K | $2.00 / 1M"), "{sent}");
    let e = sink.wait_events(1).await.remove(0);
    assert_eq!(
        (e.organisation_id.as_str(), e.workspace_id.as_str(), e.key_id.as_str()),
        ("org_system", "ws_system", "price-tracker")
    );
    assert_eq!(e.trace_id, run_id);

    // The model took the page's figures, and the page owns them now.
    let (_, m) = gw.api("GET", &format!("/models/{target}"), None).await;
    assert_eq!(
        (m["context_window"].as_u64(), m["capabilities"]["vision"].as_bool()),
        (Some(400_000), Some(true)),
        "{m}"
    );
    assert_eq!(m["facts_from"]["context_window"], page.as_str(), "{m}");
    let (_, log) = gw.api("GET", "/audit-log?limit=50", None).await;
    let written = log.to_string();
    assert!(written.contains("update_from_price_page") && written.contains(&target), "{log}");

    // An administrator changes the window by hand: the field is theirs again, and a reading of
    // the unchanged page does not undo it.
    let (_, mut edit) = gw.api("GET", &format!("/models/{target}"), None).await;
    edit["context_window"] = json!(200_000);
    let (st, v) = gw.api("PUT", &format!("/models/{target}"), Some(edit)).await;
    assert_eq!(st, 200, "{v}");
    assert!(v["facts_from"].get("context_window").is_none(), "{v}");
    script(&mock, vec![reply(target_page(400_000, 2.5))]);
    assert_eq!(gw.api("POST", &format!("/price-pages/{page}/check"), None).await.0, 202);
    let (run_id, _) = check_run(&gw).await;
    let (_, r) = internal(&gw, TOKEN, &format!("/{run_id}/readings"), changed("h1")).await;
    let t = outcome(&r, "openai/target");
    assert_eq!((t["outcome"].as_str(), t.get("properties")), (Some("unchanged"), None), "{r}");
    finish(&gw, &run_id).await;
    assert_eq!(gw.api("GET", &format!("/models/{target}"), None).await.1["context_window"], 200_000);

    // The page itself changes its window: that is applied.
    script(&mock, vec![reply(target_page(1_000_000, 2.5))]);
    assert_eq!(gw.api("POST", "/price-job/run", None).await.0, 202);
    let (run_id, _) = check_run(&gw).await;
    let (_, r) = internal(&gw, TOKEN, &format!("/{run_id}/readings"), changed("h2")).await;
    assert_eq!(outcome(&r, "openai/target")["properties"], json!(["context_window"]), "{r}");
    finish(&gw, &run_id).await;
    assert_eq!(gw.api("GET", &format!("/models/{target}"), None).await.1["context_window"], 1_000_000);

    // An answer in prose fails the page; it is read again at the next check.
    use base64::Engine as _;
    let prose = format!("reply={}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("I found no prices."));
    script(&mock, vec![prose]);
    assert_eq!(gw.api("POST", "/price-job/run", None).await.0, 202);
    let (run_id, _) = check_run(&gw).await;
    let (_, r) = internal(&gw, TOKEN, &format!("/{run_id}/readings"), changed("h3")).await;
    assert_eq!(r["outcome"], "failed", "{r}");
    assert!(r["message"].as_str().unwrap().contains("\"models\""), "{r}");
    finish(&gw, &run_id).await;
    let (_, pages) = gw.api("GET", "/price-pages", None).await;
    assert_eq!(
        (pages[0]["state"]["consecutive_failures"].as_u64(), pages[0]["state"]["outcome"].as_str()),
        (Some(1), Some("failed"))
    );
}

#[tokio::test]
async fn a_long_page_is_read_in_parts_within_the_runs_budget() {
    let (gw, mock, _sink, fx) = setup().await;
    // A small window: the page is read in parts of 24,000 characters.
    let reader = json!({ "capabilities": { "structured_output": true }, "context_window": 32000 });
    gw.priced_model(&fx, "openai/extractor", "extractor", reader, rates()).await;
    gw.priced_model(&fx, "openai/far", "far", json!({}), rates()).await;
    let page = gw
        .create("/price-pages", json!({ "provider_id": fx.provider, "url": "https://provider.example/pricing" }))
        .await
        .id;
    assert_eq!(
        gw.api("PUT", "/price-job", Some(json!({ "enabled": true, "extraction_model": "openai/extractor" }))).await.0,
        200
    );
    let mut text = String::from("Model | Input | Output\n");
    for i in 0..1800 {
        text.push_str(&format!("other-model-{i:04} | $1.00 / 1M | $2.00 / 1M\n"));
    }
    text.push_str("far | $3.00 / 1M | $6.00 / 1M\n");
    let parts = text.len().div_ceil(24_000);
    assert!(parts >= 3, "{parts}");
    let mut replies: Vec<String> = (1..parts).map(|_| reply(json!([]))).collect();
    replies.push(reply(json!([{ "model": "far", "api_id_stated": true, "input": 3.0, "output": 6.0 }])));
    script(&mock, replies);
    let (run_id, _) = check_run(&gw).await;
    let reading = json!({ "page_id": page, "outcome": "changed", "content_hash": "h", "text": text });
    let (_, r) = internal(&gw, TOKEN, &format!("/{run_id}/readings"), reading.clone()).await;
    assert_eq!(r["outcome"], "read", "{r}");
    assert_eq!(outcome(&r, "openai/far")["outcome"], "applied", "the model in the last part: {r}");
    let calls = mock.captured().len();
    assert_eq!(calls, parts, "one call per part");
    let header = mock.captured().last().unwrap().json().unwrap()["messages"][0]["content"].as_str().unwrap().to_owned();
    assert!(
        header.contains("Model | Input | Output") && header.contains(&format!("part {parts} of {parts}")),
        "the header comes along"
    );
    finish(&gw, &run_id).await;

    // A run may call the extraction model only so often: a page beyond its budget waits.
    assert_eq!(
        gw.api(
            "PUT",
            "/price-job",
            Some(json!({ "enabled": true, "extraction_model": "openai/extractor", "max_extractions_per_run": 2 }))
        )
        .await
        .0,
        200
    );
    assert_eq!(gw.api("POST", &format!("/price-pages/{page}/check"), None).await.0, 202);
    let (run_id, _) = check_run(&gw).await;
    let (_, r) = internal(&gw, TOKEN, &format!("/{run_id}/readings"), reading).await;
    assert_eq!(r["outcome"], "not_read", "{r}");
    assert!(r["message"].as_str().unwrap().contains("budget"), "{r}");
    assert_eq!(mock.captured().len(), calls, "no call was made");
}

#[tokio::test]
async fn activating_a_model_prices_it_from_what_the_pages_said() {
    let (gw, _mock, _sink, fx) = setup().await;
    let page = gw
        .create(
            "/price-pages",
            json!({ "provider_id": fx.provider, "kind": "feed", "url": "https://prices.example/list.json" }),
        )
        .await
        .id;
    assert_eq!(gw.api("PUT", "/price-job", Some(json!({ "enabled": true }))).await.0, 200);
    let (run_id, _) = check_run(&gw).await;
    let reading = feed_reading(&page, "h1", vec![priced("later", 1.5, 6.0)]);
    assert_eq!(internal(&gw, TOKEN, &format!("/{run_id}/readings"), reading).await.0, 200);
    finish(&gw, &run_id).await;

    // A model the feed already named is priced from it the moment it is activated, without a
    // check: the hand-entered price that let it be activated is replaced.
    let later = gw.priced_model(&fx, "openai/later", "later", json!({}), rates()).await;
    let (_, prices) = gw.api("GET", &format!("/models/{later}/prices"), None).await;
    let now = prices.as_array().unwrap().iter().find(|p| p["in_effect"] == true).unwrap();
    assert_eq!((now["input"].as_f64(), now["source"].as_str()), (Some(1.5), Some("feed")), "{prices}");
    let (_, l) =
        internal(&gw, TOKEN, "/lease", json!({ "job": "price_check", "holder": "scraper-1", "ttl_seconds": 120 }))
            .await;
    assert_eq!(l["due"], false, "no check was needed: {l}");
    // One the feed does not name asks for a check of the provider's pages.
    gw.priced_model(&fx, "openai/unknown", "unknown", json!({}), rates()).await;
    let (run_id, work) = check_run(&gw).await;
    assert_eq!(work["pages"][0]["force"], true);
    finish(&gw, &run_id).await;
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
