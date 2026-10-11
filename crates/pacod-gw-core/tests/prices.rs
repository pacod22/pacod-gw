//! The price tracker (concept section 13.3) from the core's side: its configuration, the
//! lease, and what the core decides for every price that is found. The test plays the part of
//! the analytics service on the internal API.

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

fn lease_request() -> Value {
    json!({ "job": "prices", "holder": "analytics-1", "ttl_seconds": 120 })
}

fn candidate(model: &str, input: f64, output: f64) -> Value {
    json!({ "model_id": model, "currency": "USD", "input": input, "output": output, "cache_read": null,
            "cache_write_5m": null, "cache_write_1h": null, "source": "feed",
            "evidence_url": "https://prices.example/list.json", "evidence_text": "input 2.00, output 10.00" })
}

fn rates() -> Value {
    json!({ "currency": "USD", "input": 2.0, "output": 10.0 })
}

#[tokio::test]
async fn a_run_applies_small_changes_proposes_large_ones_and_leaves_manual_prices_alone() {
    let (gw, _mock, _sink, fx) = setup().await;
    // Four models: one priced by hand, two by an earlier feed, one a draft waiting for a price.
    let manual = gw.priced_model(&fx, "openai/manual", "manual", json!({}), rates()).await;
    let _ = manual;
    let steady = gw.draft_model(&fx.provider, "openai/steady", "steady", json!({})).await;
    let moved = gw.draft_model(&fx.provider, "openai/moved", "moved", json!({})).await;
    let fresh = gw.draft_model(&fx.provider, "openai/fresh", "fresh", json!({})).await;
    let _ = fresh;
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
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["counts"], json!({ "applied": 2 }), "an import is the administrator's own word and is applied");
    gw.activate(&steady).await;
    gw.activate(&moved).await;

    // The job is configured and switched on; nothing is due until its time, or until asked.
    let job = json!({ "enabled": true, "schedule": "0 3 * * *", "time_zone": "Europe/Vienna", "auto_apply_bound": 0.3,
                      "stale_days": 30, "sources": [{ "provider_id": fx.provider, "kind": "feed", "url": "https://prices.example/list.json" }] });
    let (st, v) = gw.api("PUT", "/price-job", Some(job.clone())).await;
    assert_eq!(st, 200, "{v}");
    assert!(v["next_run_at"].is_string() && v["running"] == false);
    for bad in [
        json!({ "schedule": "every day" }),
        json!({ "time_zone": "Mars/Olympus" }),
        json!({ "auto_apply_bound": -1.0 }),
        json!({ "sources": [{ "provider_id": "prv_nope", "kind": "feed", "url": "https://x.example" }] }),
        json!({ "sources": [{ "provider_id": fx.provider, "kind": "feed", "url": "ftp://x.example" }] }),
        json!({ "sources": [{ "provider_id": fx.provider, "kind": "page", "url": "https://x.example" }] }),
    ] {
        let mut body = job.clone();
        body.as_object_mut().unwrap().extend(bad.as_object().unwrap().clone());
        let (st, v) = gw.api("PUT", "/price-job", Some(body)).await;
        assert_eq!(st, 422, "{bad}: {v}");
    }

    // The internal API is for the analytics service only.
    assert_eq!(internal(&gw, "wrong-token", "/lease", lease_request()).await.0, 401);
    // And the internal token opens nothing else: it is no admin of the management API.
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
    // The import counted as the job's last start, so the schedule is not due yet.
    let (st, l) = internal(&gw, TOKEN, "/lease", lease_request()).await;
    assert_eq!((st, l["granted"].as_bool(), l["due"].as_bool()), (200, Some(true), Some(false)), "{l}");
    // A second instance does not get the lease while the first holds it.
    let (_, other) =
        internal(&gw, TOKEN, "/lease", json!({ "job": "prices", "holder": "analytics-2", "ttl_seconds": 120 })).await;
    assert_eq!(other["granted"], false);

    // "Run now".
    assert_eq!(gw.api("POST", "/price-job/run", None).await.0, 202);
    let (_, l) = internal(&gw, TOKEN, "/lease", lease_request()).await;
    assert_eq!((l["granted"].as_bool(), l["due"].as_bool()), (Some(true), Some(true)), "{l}");
    let run_id = l["run_id"].as_str().unwrap().to_owned();
    let models = l["work"]["models"].as_array().unwrap();
    let names: Vec<&str> = models.iter().map(|m| m["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        ["openai/fresh", "openai/manual", "openai/moved", "openai/steady"],
        "active models and drafts waiting for a price; free models are not tracked"
    );
    assert_eq!(l["work"]["config"]["sources"][0]["url"], "https://prices.example/list.json");
    assert!(models[0]["current"].is_null() && models[2]["current"]["rates"]["input"] == 2.0);
    assert_eq!(gw.api("GET", "/price-job", None).await.1["running"], true);

    // What the tracker found, and what the core makes of it.
    let found = json!({ "run_id": run_id, "job": "prices", "finished_at": "2026-10-05T12:00:00Z", "ok": true, "message": "",
        "result": { "candidates": [
            candidate("openai/manual", 3.0, 15.0),   // entered by hand: never overwritten
            candidate("openai/steady", 2.2, 10.0),   // +10 %: within the bound
            candidate("openai/moved", 4.0, 10.0),    // +100 %: needs approval
            candidate("openai/fresh", 1.0, 4.0),     // a first price: needs approval
            candidate("openai/nothing", 1.0, 4.0),   // not a model
            json!({ "model_id": "openai/steady", "currency": "USD", "input": 9.0, "output": 9.0, "source": "feed", "evidence_url": "", "evidence_text": "" })
        ] } });
    let (st, r) = internal(&gw, TOKEN, &format!("/{run_id}/result"), found.clone()).await;
    assert_eq!(st, 200, "{r}");
    let by = |name: &str| {
        r["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["model"] == name)
            .cloned()
            .unwrap_or_else(|| panic!("{name} missing in {r}"))
    };
    assert_eq!(by("openai/manual")["outcome"], "skipped_manual");
    assert_eq!(
        (by("openai/steady")["outcome"].as_str(), by("openai/steady")["version"].as_u64()),
        (Some("applied"), Some(2))
    );
    assert!((by("openai/steady")["change"].as_f64().unwrap() - 0.1).abs() < 1e-9);
    assert_eq!(by("openai/moved")["outcome"], "proposed");
    assert!(by("openai/moved")["detail"].as_str().unwrap().contains("100 %"));
    assert_eq!(
        (by("openai/fresh")["outcome"].as_str(), by("openai/fresh")["detail"].as_str()),
        (Some("proposed"), Some("the model's first price"))
    );
    assert_eq!(by("openai/nothing")["outcome"], "invalid");
    assert_eq!(r["counts"]["proposed"], 2);
    // A result cannot be posted twice.
    assert_eq!(internal(&gw, TOKEN, &format!("/{run_id}/result"), found).await.0, 409);

    // Applied: the price in effect changed, with its source and evidence.
    let (_, current) = gw.api("GET", "/prices", None).await;
    let price = |name: &str| current.as_array().unwrap().iter().find(|p| p["model"] == name).cloned();
    let steady_now = price("openai/steady").unwrap();
    assert_eq!(
        (steady_now["input"].as_f64(), steady_now["source"].as_str(), steady_now["version"].as_u64()),
        (Some(2.2), Some("feed"), Some(2))
    );
    assert_eq!(steady_now["evidence_url"], "https://prices.example/list.json");
    assert_eq!(price("openai/moved").unwrap()["input"], 2.0, "a proposal changes nothing until it is approved");
    assert_eq!(price("openai/manual").unwrap()["input"], 2.0);

    // The run is on record.
    let (_, runs) = gw.api("GET", "/price-job/runs", None).await;
    let last = &runs[0];
    assert_eq!(
        (last["id"].as_str(), last["trigger"].as_str(), last["ok"].as_bool()),
        (Some(run_id.as_str()), Some("manual"), Some(true))
    );
    assert_eq!(last["result"]["counts"]["applied"], 1);
    assert_eq!(runs[1]["trigger"], "import");
    assert_eq!(gw.api("GET", "/price-job", None).await.1["running"], false);

    // Proposals: approve one, reject the other.
    let (_, proposals) = gw.api("GET", "/price-proposals", None).await;
    let list = proposals.as_array().unwrap();
    assert_eq!(list.len(), 2);
    let id_of = |name: &str| list.iter().find(|p| p["model"] == name).unwrap()["id"].as_str().unwrap().to_owned();
    let (st, approved) = gw.api("POST", &format!("/price-proposals/{}/approve", id_of("openai/moved")), None).await;
    assert_eq!(
        (st, approved["status"].as_str(), approved["in_effect"].as_bool()),
        (200, Some("active"), Some(true)),
        "{approved}"
    );
    let (st, rejected) = gw.api("POST", &format!("/price-proposals/{}/reject", id_of("openai/fresh")), None).await;
    assert_eq!((st, rejected["status"].as_str()), (200, Some("rejected")));
    assert_eq!(gw.api("POST", &format!("/price-proposals/{}/approve", id_of("openai/fresh")), None).await.0, 409);
    let (_, current) = gw.api("GET", "/prices", None).await;
    let moved_now = current.as_array().unwrap().iter().find(|p| p["model"] == "openai/moved").cloned().unwrap();
    assert_eq!((moved_now["input"].as_f64(), moved_now["version"].as_u64()), (Some(4.0), Some(2)));
    assert!(gw.api("GET", "/price-proposals", None).await.1.as_array().unwrap().is_empty());

    // The next run finds the same figures: nothing changes, and a model nobody priced is named.
    assert_eq!(gw.api("POST", "/price-job/run", None).await.0, 202);
    let (_, l) = internal(&gw, TOKEN, "/lease", lease_request()).await;
    let run_id = l["run_id"].as_str().unwrap().to_owned();
    let again = json!({ "run_id": run_id, "job": "prices", "finished_at": "2026-10-06T12:00:00Z", "ok": true, "message": "",
        "result": { "candidates": [candidate("openai/steady", 2.2, 10.0), candidate("openai/moved", 4.0, 10.0)] } });
    let (_, r) = internal(&gw, TOKEN, &format!("/{run_id}/result"), again.clone()).await;
    assert_eq!(r["counts"], json!({ "no_price": 2, "unchanged": 2 }), "{r}");

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
    assert_eq!(stale(&alerts), ["openai/manual", "openai/moved", "openai/steady"], "{alerts}");

    // The tracker finds the same figures again. That is a check: the prices it looked at
    // are confirmed and no longer stale. The one entered by hand is not its to confirm.
    assert_eq!(gw.api("POST", "/price-job/run", None).await.0, 202);
    let (_, l) = internal(&gw, TOKEN, "/lease", lease_request()).await;
    let run_id = l["run_id"].as_str().unwrap().to_owned();
    let mut third = again;
    third["run_id"] = json!(run_id);
    let (_, r) = internal(&gw, TOKEN, &format!("/{run_id}/result"), third).await;
    assert_eq!(r["counts"]["unchanged"], 2, "{r}");
    let (_, alerts) = gw.api("GET", "/monitoring/alerts", None).await;
    assert_eq!(stale(&alerts), ["openai/manual"], "{alerts}");
    let (_, versions) = gw.api("GET", &format!("/models/{steady}/prices"), None).await;
    let active = versions.as_array().unwrap().iter().find(|p| p["in_effect"] == true).unwrap();
    assert!(active["confirmed_at"].is_string(), "{active}");
    assert_eq!(
        (active["version"].as_u64(), active["input"].as_f64()),
        (Some(2), Some(2.2)),
        "a confirmation is not a new version"
    );
}

#[tokio::test]
async fn extraction_goes_through_the_gateways_own_data_plane() {
    let (gw, mock, sink, fx) = setup().await;
    gw.priced_model(
        &fx,
        "openai/extractor",
        "extractor",
        json!({ "capabilities": { "structured_output": true } }),
        rates(),
    )
    .await;
    // The extraction template lives in the built-in _system workspace, which may use every model.
    gw.create(
        "/orgs/org_system/workspaces/ws_system/templates",
        json!({ "name": "price-reader", "spec": { "model": "openai/extractor", "max_output_tokens": 2000 } }),
    )
    .await;
    let job = json!({ "enabled": false, "schedule": "0 3 * * *", "time_zone": "UTC", "auto_apply_bound": 0.3, "stale_days": 30,
                      "sources": [{ "provider_id": fx.provider, "kind": "page", "url": "https://provider.example/pricing" }],
                      "extraction": { "template": "@price-reader" } });
    let mut elsewhere = job.clone();
    elsewhere["extraction"]["workspace_id"] = json!(fx.ws);
    let (st, v) = gw.api("PUT", "/price-job", Some(elsewhere)).await;
    assert_eq!(st, 422, "only _system: {v}");
    // A source is fetched from inside the network, so it has to be a public address.
    let mut inside = job.clone();
    inside["sources"][0]["url"] = json!("http://169.254.169.254/latest/meta-data/");
    let (st, v) = gw.api("PUT", "/price-job", Some(inside)).await;
    assert_eq!(st, 422, "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("link-local"), "{v}");
    let (st, v) = gw.api("PUT", "/price-job", Some(job)).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["extraction"]["template"], "price-reader");
    assert_eq!(gw.api("POST", "/price-job/run", None).await.0, 202);
    let (_, l) = internal(&gw, TOKEN, "/lease", lease_request()).await;
    let run_id = l["run_id"].as_str().unwrap().to_owned();

    let (st, found) = internal(
        &gw,
        TOKEN,
        &format!("/{run_id}/extract"),
        json!({ "url": "https://provider.example/pricing", "text": "extractor: $2.00 per 1M input tokens, $10.00 per 1M output tokens", "models": ["extractor"] }),
    )
    .await;
    assert_eq!(st, 200, "{found}");
    // The mock answers a schema with placeholder values; what matters is the way the call took.
    assert!(found["prices"].is_array(), "{found}");
    let sent = mock.captured()[0].json().unwrap();
    assert_eq!(sent["model"], "extractor");
    assert_eq!(sent["response_format"]["type"], "json_schema");
    assert!(sent["messages"][0]["content"].as_str().unwrap().contains("$2.00 per 1M input tokens"));

    // It is accounted like any other call, to the gateway's own workspace.
    let e = sink.wait_events(1).await.remove(0);
    assert_eq!(
        (e.organisation_id.as_str(), e.workspace_id.as_str(), e.key_id.as_str(), e.template.as_str()),
        ("org_system", "ws_system", "price-tracker", "price-reader")
    );
    assert_eq!((e.trace_id.as_str(), e.interface.as_str()), (run_id.as_str(), "native"));
    assert!(e.cost.unwrap() > 0.0);

    // A model that wraps its answer in a code fence is read all the same; one that answers in
    // prose is an error, not an empty list.
    use base64::Engine as _;
    let reply = |text: &str| format!("reply={}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(text));
    let answer = r#"{"prices":[{"model":"extractor","currency":"USD","input":2.0,"evidence":"$2.00",
        "off_peak":{"input":1.0,"peak":{"weekdays":[1,2,3,4,5],"hours":["09:00-18:00"],"utc_offset":"+08:00"}}}]}"#;
    mock.set_script(
        pacod_gw_testkit::mock::Behavior::parse_all([
            reply(&format!("Here are the prices:\n```json\n{answer}\n```")),
            reply("I could not find any prices on this page."),
        ])
        .unwrap(),
    );
    let ask = json!({ "url": "https://provider.example/pricing", "text": "page", "models": ["extractor"] });
    let (st, found) = internal(&gw, TOKEN, &format!("/{run_id}/extract"), ask.clone()).await;
    assert_eq!(st, 200, "{found}");
    assert_eq!(found["prices"][0]["off_peak"]["peak"]["utc_offset"], "+08:00", "passed on as stated: {found}");
    let (st, error) = internal(&gw, TOKEN, &format!("/{run_id}/extract"), ask).await;
    assert_eq!(st, 502, "{error}");
    assert_eq!(error["error"]["type"], "extraction_unreadable");
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
async fn activating_a_model_prices_it_alone_until_a_full_run_is_asked_for() {
    let (gw, _mock, _sink, fx) = setup().await;
    let job = json!({ "enabled": true, "schedule": "0 3 * * *", "time_zone": "UTC", "auto_apply_bound": 0.3,
                      "stale_days": 30, "sources": [{ "provider_id": fx.provider, "kind": "feed", "url": "https://prices.example/list.json" }] });
    assert_eq!(gw.api("PUT", "/price-job", Some(job)).await.0, 200);
    let done = |run_id: &str| {
        json!({ "run_id": run_id, "job": "prices", "finished_at": "2026-10-05T12:00:00Z", "ok": true, "message": "",
                "result": { "candidates": [] } })
    };
    // The job has never run, so its first run is due and covers everything.
    let (_, l) = internal(&gw, TOKEN, "/lease", lease_request()).await;
    let first = l["run_id"].as_str().unwrap().to_owned();
    internal(&gw, TOKEN, &format!("/{first}/result"), done(&first)).await;

    // A model is activated: a run for it alone.
    let alone = gw.priced_model(&fx, "openai/alone", "alone", json!({}), rates()).await;
    let (_, l) = internal(&gw, TOKEN, "/lease", lease_request()).await;
    assert_eq!(l["due"], true, "{l}");
    let run_id = l["run_id"].as_str().unwrap().to_owned();
    let names = |l: &Value| -> Vec<String> {
        l["work"]["models"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap().to_owned()).collect()
    };
    assert_eq!(names(&l), ["openai/alone"]);
    // Another activation while the run is open waits for the next run; the open run, handed out
    // again, still covers its own model only.
    let later = gw.priced_model(&fx, "openai/later", "later", json!({}), rates()).await;
    let (_, again) = internal(&gw, TOKEN, "/lease", lease_request()).await;
    assert_eq!((again["run_id"].as_str(), names(&again)), (Some(run_id.as_str()), vec!["openai/alone".to_owned()]));
    // Its result names only its model, not every model nothing was found for.
    let (_, r) = internal(&gw, TOKEN, &format!("/{run_id}/result"), done(&run_id)).await;
    let reported: Vec<&str> = r["models"].as_array().unwrap().iter().map(|m| m["model"].as_str().unwrap()).collect();
    assert_eq!(reported, ["openai/alone"], "{r}");
    let (_, runs) = gw.api("GET", "/price-job/runs", None).await;
    assert_eq!((runs[0]["trigger"].as_str(), runs[0]["models"].clone()), (Some("activation"), json!([alone])));

    // Asking for a full run covers the pending model too.
    assert_eq!(gw.api("POST", "/price-job/run", None).await.0, 202);
    let (_, l) = internal(&gw, TOKEN, "/lease", lease_request()).await;
    let all = names(&l);
    assert!(all.contains(&"openai/alone".to_owned()) && all.contains(&"openai/later".to_owned()), "{all:?}");
    let full = l["run_id"].as_str().unwrap().to_owned();
    internal(&gw, TOKEN, &format!("/{full}/result"), done(&full)).await;
    let (_, l) = internal(&gw, TOKEN, "/lease", lease_request()).await;
    assert_eq!(l["due"], false, "nothing is left to do: {l}");
    let _ = later;
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
