//! Discovery that reads what providers report about their models, and prices with long-context
//! tiers and off-peak hours (concept sections 6.4 and 13.1), end to end.

mod common;

use common::{IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

async fn run_discovery(gw: &TestGw) -> Value {
    let (st, _) = gw.api("POST", "/discovery/runs", None).await;
    assert_eq!(st, 202);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let (_, runs) = gw.api("GET", "/discovery/runs", None).await;
        if runs[0]["finished_at"].is_string() {
            return runs[0]["result"].clone();
        }
        assert!(std::time::Instant::now() < deadline, "{runs}");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// Asks for a run and waits for it: the first finished run not among `known`.
async fn next_run(gw: &TestGw, known: &[&str]) -> Value {
    let (st, _) = gw.api("POST", "/discovery/runs", None).await;
    assert_eq!(st, 202);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let (_, runs) = gw.api("GET", "/discovery/runs", None).await;
        let next = runs
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["finished_at"].is_string() && !known.contains(&r["id"].as_str().unwrap_or_default()));
        if let Some(run) = next {
            return run.clone();
        }
        assert!(std::time::Instant::now() < deadline, "{runs}");
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

async fn proposal(gw: &TestGw, provider: &str, upstream: &str) -> Value {
    let (_, open) = gw.api("GET", "/discovery/proposals", None).await;
    open.as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider_id"] == provider && p["upstream_id"] == upstream && p["kind"] == "new")
        .unwrap_or_else(|| panic!("no proposal for {upstream}: {open}"))
        .clone()
}

#[tokio::test]
async fn discovery_reads_what_providers_report_and_suggests_a_preset() {
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(None).await;
    let deepseek = gw
        .create("/providers", json!({ "name": "deepseek", "family": "openai", "base_url": format!("{}/emulate/deepseek", mock.base_url()) }))
        .await
        .id;
    gw.create(&format!("/providers/{deepseek}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
    let ollama = gw
        .create(
            "/providers",
            json!({ "name": "ollama", "family": "ollama", "base_url": format!("{}/v1", mock.base_url()) }),
        )
        .await
        .id;

    let result = run_discovery(&gw).await;
    assert_eq!(result["deepseek"]["new"], 2, "{result}");
    assert_eq!(result["ollama"]["new"], 2, "{result}");

    // DeepSeek says which model takes images: the vision preset, under the provider's own id.
    let p = proposal(&gw, &deepseek, "deepseek-flash").await;
    assert_eq!(p["suggested_preset"], "deepseek-vision");
    // The list narrows to one provider.
    let (_, only) = gw.api("GET", &format!("/discovery/proposals?provider_id={ollama}"), None).await;
    assert!(only.as_array().unwrap().iter().all(|p| p["provider_id"] == ollama.as_str()), "{only}");
    assert_eq!(
        (p["details"]["vision"].as_bool(), p["details"]["context_window"].as_u64()),
        (Some(true), Some(1_048_576))
    );
    let (st, m) = gw
        .api(
            "POST",
            &format!("/discovery/proposals/{}/accept", p["id"].as_str().unwrap()),
            Some(json!({ "naming": "bare" })),
        )
        .await;
    assert_eq!(st, 201, "{m}");
    assert_eq!(m["name"], "deepseek-flash");
    assert_eq!(
        (m["capabilities"]["vision"].as_bool(), m["capabilities"]["tool_choice_forced"].as_bool()),
        (Some(true), Some(false))
    );
    let p = proposal(&gw, &deepseek, "deepseek-v4-pro").await;
    assert_eq!((p["suggested_preset"].as_str(), p["suggested_kind"].as_str()), (Some("deepseek"), Some("chat")));
    let (_, m) = gw.api("POST", &format!("/discovery/proposals/{}/accept", p["id"].as_str().unwrap()), None).await;
    assert_eq!(
        (m["name"].as_str(), m["capabilities"]["vision"].as_bool()),
        (Some("deepseek/deepseek-v4-pro"), Some(false))
    );

    // Ollama's own server: what each model can do, and it costs nothing.
    let p = proposal(&gw, &ollama, "qwen3:latest").await;
    assert_eq!(
        (p["details"]["reasons"].as_bool(), p["details"]["context_window"].as_u64()),
        (Some(true), Some(40_960))
    );
    let (_, m) = gw.api("POST", &format!("/discovery/proposals/{}/accept", p["id"].as_str().unwrap()), None).await;
    assert_eq!(m["capabilities"]["reasoning"], "effort", "the thinking preset: {m}");
    assert_eq!(m["free"], true);

    // Ollama says llama3.2 sees no images, but a preset the administrator chose keeps its own
    // vision setting (the `ollama` preset's is on); the kind is theirs too.
    let p = proposal(&gw, &ollama, "llama3.2:latest").await;
    assert_eq!(p["details"]["vision"], false);
    let (st, m) = gw
        .api(
            "POST",
            &format!("/discovery/proposals/{}/accept", p["id"].as_str().unwrap()),
            Some(json!({ "capability_preset": "ollama", "kind": "chat" })),
        )
        .await;
    assert_eq!(st, 201, "{m}");
    assert_eq!((m["kind"].as_str(), m["capabilities"]["vision"].as_bool()), (Some("chat"), Some(true)));

    // A model created directly gets the suggestion too.
    let (st, m) = gw
        .api(
            "POST",
            "/models",
            Some(json!({ "name": "deepseek/pro", "provider_id": deepseek, "upstream_id": "deepseek-v4-pro" })),
        )
        .await;
    assert_eq!(st, 201, "{m}");
    assert_eq!(m["capabilities"]["structured_output"], false, "the deepseek preset: {m}");
}

async fn accept(gw: &TestGw, p: &Value) -> Value {
    let (st, m) = gw.api("POST", &format!("/discovery/proposals/{}/accept", p["id"].as_str().unwrap()), None).await;
    assert_eq!(st, 201, "{m}");
    m
}

#[tokio::test]
async fn discovery_takes_over_what_each_provider_reports() {
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(None).await;
    let provider = |body: Value| {
        let gw = &gw;
        async move {
            let id = gw.create("/providers", body).await.id;
            gw.create(&format!("/providers/{id}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
            id
        }
    };
    let anthropic = provider(json!({ "name": "anthropic", "family": "anthropic", "base_url": mock.base_url() })).await;
    let gemini = provider(json!({ "name": "gemini", "family": "gemini", "managed_api": "native",
                                  "base_url": format!("{}/emulate/gemini/v1beta", mock.base_url()) }))
    .await;
    let deepseek = provider(
        json!({ "name": "deepseek", "family": "openai", "base_url": format!("{}/emulate/deepseek", mock.base_url()) }),
    )
    .await;
    let ollama = gw
        .create(
            "/providers",
            json!({ "name": "ollama", "family": "ollama", "base_url": format!("{}/v1", mock.base_url()) }),
        )
        .await
        .id;
    let result = run_discovery(&gw).await;
    assert_eq!(result["anthropic"]["new"], 3, "{result}");

    // Anthropic's capability tree: the thinking it takes picks the preset.
    let p = proposal(&gw, &anthropic, "claude-haiku-4-5").await;
    assert_eq!(
        (p["suggested_preset"].as_str(), p["details"]["reasoning"].as_str()),
        (Some("anthropic-budget"), Some("budget"))
    );
    assert_eq!(p["details"]["structured_output"], true);
    let p = proposal(&gw, &anthropic, "claude-opus-5-5").await;
    assert_eq!(p["suggested_preset"], "anthropic-adaptive");
    let m = accept(&gw, &p).await;
    assert_eq!(m["capabilities"]["structured_output"], true);
    assert!(
        m["capabilities"]["reasoning_efforts"].as_array().unwrap().iter().any(|e| e == "xhigh"),
        "a level the list does not name stays: {m}"
    );

    // Gemini's list: the sampling defaults and the highest temperature.
    let p = proposal(&gw, &gemini, "gemini-3.8-flash").await;
    assert_eq!(p["details"]["sampling"]["temperature"], json!({ "max": 2.0, "default": 1.0 }));
    let m = accept(&gw, &p).await;
    assert_eq!(m["capabilities"]["sampling"]["top_k"]["default"], 64.0);

    // Ollama's show: tools and the model file's defaults.
    let p = proposal(&gw, &ollama, "qwen3:latest").await;
    assert_eq!(p["details"]["tools"], true);
    let m = accept(&gw, &p).await;
    assert_eq!(m["capabilities"]["sampling"]["temperature"]["default"], 0.6);

    // DeepSeek's effort levels and default; `none` stays, it is the gateway's own.
    let p = proposal(&gw, &deepseek, "deepseek-v4-pro").await;
    assert_eq!(p["details"]["default_effort"], "high");
    let m = accept(&gw, &p).await;
    assert_eq!(m["capabilities"]["default_effort"], "high");
    assert_eq!(m["capabilities"]["reasoning_efforts"], json!(["none", "low", "high", "max"]));
    assert_eq!(m["capabilities"]["json_mode"], true);
}

async fn open_update(gw: &TestGw, provider: &str, upstream: &str) -> Option<Value> {
    let (_, open) = gw.api("GET", "/discovery/proposals", None).await;
    open.as_array()
        .unwrap()
        .iter()
        .find(|p| p["provider_id"] == provider && p["upstream_id"] == upstream && p["kind"] == "update")
        .cloned()
}

#[tokio::test]
async fn changed_facts_become_an_update_an_admin_applies() {
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(None).await;
    let deepseek = gw
        .create("/providers", json!({ "name": "deepseek", "family": "openai", "base_url": format!("{}/emulate/deepseek", mock.base_url()) }))
        .await
        .id;
    gw.create(&format!("/providers/{deepseek}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
    // A model stored before its preset had JSON mode, with a stale context window and images.
    let (_, presets) = gw.api("GET", "/capability-presets", None).await;
    let mut caps =
        presets.as_array().unwrap().iter().find(|p| p["name"] == "deepseek").unwrap()["capabilities"].clone();
    caps.as_object_mut().unwrap().remove("json_mode");
    caps["vision"] = json!(true);
    let model = gw
        .create(
            "/models",
            json!({ "name": "deepseek/pro", "provider_id": deepseek, "upstream_id": "deepseek-v4-pro",
                    "context_window": 128000, "capabilities": caps }),
        )
        .await
        .id;

    let result = run_discovery(&gw).await;
    assert_eq!(result["deepseek"]["updated"], 1, "{result}");
    let p = open_update(&gw, &deepseek, "deepseek-v4-pro").await.expect("an update proposal");
    assert_eq!(p["model_id"], model.as_str());
    let changes = &p["details"]["changes"];
    assert_eq!(changes["context_window"], json!({ "current": 128000, "reported": 1_048_576, "source": "provider" }));
    assert_eq!(changes["capabilities.vision"]["reported"], false);
    assert_eq!(changes["capabilities.json_mode"], json!({ "current": false, "reported": true, "source": "preset" }));
    assert_eq!(changes["capabilities.default_effort"]["reported"], "high");

    // Some of it is applied, through the model's own rules, into the audit log.
    let id = p["id"].as_str().unwrap().to_owned();
    let (st, m) = gw
        .api(
            "POST",
            &format!("/discovery/proposals/{id}/apply"),
            Some(json!({ "fields": ["context_window", "capabilities.json_mode"] })),
        )
        .await;
    assert_eq!(st, 200, "{m}");
    assert_eq!((m["context_window"].as_u64(), m["capabilities"]["json_mode"].as_bool()), (Some(1_048_576), Some(true)));
    assert_eq!(m["capabilities"]["vision"], true, "not asked for: unchanged");
    let (_, log) = gw.api("GET", "/audit-log?limit=10", None).await;
    let entry =
        log["items"].as_array().unwrap().iter().find(|e| e["action"] == "update_from_discovery").expect("audited");
    assert_eq!(entry["object_id"], model.as_str());
    assert_eq!(gw.api("POST", &format!("/discovery/proposals/{id}/apply"), None).await.0, 409, "no longer open");

    // The next run proposes what is left, and a field changed by hand since is not overwritten.
    run_again(&gw).await;
    let p = open_update(&gw, &deepseek, "deepseek-v4-pro").await.expect("what is left");
    let mut left: Vec<&str> = p["details"]["changes"].as_object().unwrap().keys().map(String::as_str).collect();
    left.sort_unstable();
    assert_eq!(left, ["capabilities.default_effort", "capabilities.vision", "max_output_tokens"]);
    let (_, current) = gw.api("GET", &format!("/models/{model}"), None).await;
    let mut edited = current.clone();
    edited["capabilities"]["vision"] = json!(false);
    let (st, v) = gw.api("PUT", &format!("/models/{model}"), Some(edited)).await;
    assert_eq!(st, 200, "{v}");
    let id = p["id"].as_str().unwrap().to_owned();
    let (st, v) = gw
        .api("POST", &format!("/discovery/proposals/{id}/apply"), Some(json!({ "fields": ["capabilities.vision"] })))
        .await;
    assert_eq!(st, 409, "{v}");

    // Dismissed, it stays dismissed while the provider reports the same.
    assert_eq!(gw.api("POST", &format!("/discovery/proposals/{id}/dismiss"), None).await.0, 204);
    run_again(&gw).await;
    assert!(open_update(&gw, &deepseek, "deepseek-v4-pro").await.is_none());
    // A difference it did not have when dismissed opens it again.
    let (_, current) = gw.api("GET", &format!("/models/{model}"), None).await;
    let mut edited = current.clone();
    edited["context_window"] = json!(64000);
    assert_eq!(gw.api("PUT", &format!("/models/{model}"), Some(edited)).await.0, 200);
    run_again(&gw).await;
    let p = open_update(&gw, &deepseek, "deepseek-v4-pro").await.expect("open again");
    assert_eq!(p["details"]["changes"]["context_window"]["current"], 64000);
}

#[tokio::test]
async fn models_accepted_as_suggested_raise_no_updates() {
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(None).await;
    for (name, family, base, api) in [
        ("anthropic", "anthropic", mock.base_url(), None),
        ("gemini", "gemini", format!("{}/emulate/gemini/v1beta", mock.base_url()), Some("native")),
        ("deepseek", "openai", format!("{}/emulate/deepseek", mock.base_url()), None),
        ("ollama", "ollama", format!("{}/v1", mock.base_url()), None),
    ] {
        let mut body = json!({ "name": name, "family": family, "base_url": base });
        if let Some(api) = api {
            body["managed_api"] = json!(api);
        }
        let id = gw.create("/providers", body).await.id;
        if family != "ollama" {
            gw.create(&format!("/providers/{id}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
        }
    }
    run_discovery(&gw).await;
    let (_, open) = gw.api("GET", "/discovery/proposals", None).await;
    let new: Vec<Value> = open.as_array().unwrap().iter().filter(|p| p["kind"] == "new").cloned().collect();
    assert!(new.len() >= 9, "{open}");
    for p in &new {
        accept(&gw, p).await;
    }
    let result = run_again(&gw).await;
    for provider in ["anthropic", "gemini", "deepseek", "ollama"] {
        assert_eq!(result[provider]["updated"], 0, "{provider}: {result}");
    }
    let (_, open) = gw.api("GET", "/discovery/proposals", None).await;
    assert!(open.as_array().unwrap().iter().all(|p| p["kind"] != "update"), "{open}");
}

/// Runs discovery again: the next run after every one so far.
async fn run_again(gw: &TestGw) -> Value {
    let (_, runs) = gw.api("GET", "/discovery/runs", None).await;
    let known: Vec<String> =
        runs.as_array().unwrap().iter().filter_map(|r| r["id"].as_str().map(str::to_owned)).collect();
    let known: Vec<&str> = known.iter().map(String::as_str).collect();
    next_run(gw, &known).await["result"].clone()
}

/// A lease of the price job, as the analytics service asks for it.
async fn price_lease(gw: &TestGw) -> Value {
    let r = gw
        .http
        .post(format!("{}/api/v1/internal/jobs/lease", gw.admin))
        .header("x-gw-internal-token", "test-internal-token-0123456789")
        .json(&json!({ "job": "prices", "holder": "analytics-1", "ttl_seconds": 120 }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    r.json().await.unwrap()
}

/// Ends a run of the price job with nothing found.
async fn price_result(gw: &TestGw, run_id: &str) {
    let r = gw
        .http
        .post(format!("{}/api/v1/internal/jobs/{run_id}/result", gw.admin))
        .header("x-gw-internal-token", "test-internal-token-0123456789")
        .json(&json!({ "run_id": run_id, "job": "prices", "finished_at": "2026-11-01T12:00:00Z", "ok": true,
                       "message": "", "result": { "candidates": [] } }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn accepting_a_proposal_asks_the_price_tracker_for_the_new_draft() {
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(None).await;
    let deepseek = gw
        .create("/providers", json!({ "name": "deepseek", "family": "openai", "base_url": format!("{}/emulate/deepseek", mock.base_url()) }))
        .await
        .id;
    gw.create(&format!("/providers/{deepseek}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
    let ollama = gw
        .create(
            "/providers",
            json!({ "name": "ollama", "family": "ollama", "base_url": format!("{}/v1", mock.base_url()) }),
        )
        .await
        .id;
    let source =
        |provider: &str| json!({ "provider_id": provider, "kind": "feed", "url": "https://prices.example/list.json" });
    let job = json!({ "enabled": true, "schedule": "0 3 * * *", "time_zone": "UTC", "auto_apply_bound": 0.3,
                      "stale_days": 30, "sources": [source(&deepseek), source(&ollama)] });
    let (st, v) = gw.api("PUT", "/price-job", Some(job)).await;
    assert_eq!(st, 200, "{v}");
    // The job has never run, so its first run is due and covers everything: finished first, so
    // that what follows is a run for some models only.
    let first = price_lease(&gw).await;
    price_result(&gw, first["run_id"].as_str().unwrap()).await;
    assert_eq!(price_lease(&gw).await["due"], false);

    run_discovery(&gw).await;
    let p = proposal(&gw, &deepseek, "deepseek-flash").await;
    let (st, m) = gw.api("POST", &format!("/discovery/proposals/{}/accept", p["id"].as_str().unwrap()), None).await;
    assert_eq!(st, 201, "{m}");

    // The draft is priced at once, alone, not at the next scheduled run.
    let l = price_lease(&gw).await;
    assert_eq!(l["due"], true, "{l}");
    let names: Vec<&str> =
        l["work"]["models"].as_array().unwrap().iter().map(|m| m["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["deepseek/deepseek-flash"], "{l}");
    price_result(&gw, l["run_id"].as_str().unwrap()).await;

    // A model on one's own Ollama server is free: nothing to price.
    let p = proposal(&gw, &ollama, "qwen3:latest").await;
    let (st, m) = gw.api("POST", &format!("/discovery/proposals/{}/accept", p["id"].as_str().unwrap()), None).await;
    assert_eq!((st, m["free"].as_bool()), (201, Some(true)), "{m}");
    assert_eq!(price_lease(&gw).await["due"], false);
}

#[tokio::test]
async fn long_prompts_and_off_peak_hours_have_their_prices() {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    // A long-context tier from 10 prompt tokens, and off-peak hours around the clock.
    gw.priced_model(
        &fx,
        "openai/long",
        "long",
        json!({}),
        json!({ "currency": "USD", "input": 1.0, "output": 2.0,
                "tiers": [{ "above_input_tokens": 10, "input": 3.0, "output": 6.0 }] }),
    )
    .await;
    gw.priced_model(
        &fx,
        "openai/night",
        "night",
        json!({}),
        json!({ "currency": "USD", "input": 1.0, "output": 2.0,
                "variants": { "off_peak": { "input": 0.5, "output": 1.0 } },
                "windows": [{ "variant": "off_peak", "weekdays": [1, 2, 3, 4, 5, 6, 7], "hours_utc": ["00:00-00:00"] }] }),
    )
    .await;
    let call = |model: &'static str, text: &'static str| {
        let (http, url, key) = (gw.http.clone(), format!("{}/v1/chat/completions", gw.data), fx.key.clone());
        async move {
            let r = http
                .post(url)
                .bearer_auth(key)
                .json(&json!({ "model": model, "messages": [{ "role": "user", "content": text }] }))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), 200);
            r.json::<Value>().await.unwrap()
        }
    };
    // 8 prompt characters (2 tokens) stay under the threshold; 80 (20 tokens) are above it.
    let short = call("openai/long", "hi there").await;
    let long =
        call("openai/long", "a long prompt that runs past ten tokens, which is the threshold of the tier!!!!!").await;
    let rate = |v: &Value| {
        let u = &v["usage"];
        u["cost"]["amount"].as_f64().unwrap() * 1e6
            / (u["prompt_tokens"].as_f64().unwrap() + 2.0 * u["completion_tokens"].as_f64().unwrap())
    };
    assert!((rate(&short) - 1.0).abs() < 1e-6, "standard rates: {short}");
    assert!((rate(&long) - 3.0).abs() < 1e-6, "the tier's rates for the whole call: {long}");

    let night = call("openai/night", "hello").await;
    assert!((rate(&night) - 0.5).abs() < 1e-6, "off-peak rates: {night}");
    let events = sink.wait_events(3).await;
    assert!(events.iter().any(|e| e.price_variant == "off_peak"), "the event names the variant");

    // Windows that name no variant of the price are refused.
    let id = gw.draft_model(&fx.provider, "openai/bad", "bad", json!({})).await;
    let (st, v) = gw
        .api(
            "PUT",
            &format!("/models/{id}/price"),
            Some(json!({ "currency": "USD", "input": 1.0, "windows": [{ "variant": "off_peak", "weekdays": [1], "hours_utc": ["01:00-04:00"] }] })),
        )
        .await;
    assert_eq!(st, 422, "{v}");
}

#[tokio::test]
async fn holiday_calendars_are_shipped_checked_and_linked() {
    let gw = TestGw::start(None).await;
    // China's 2026 holidays come with the installation; DeepSeek's preset bills by them.
    let (_, settings) = gw.api("GET", "/settings", None).await;
    let cn = &settings["holiday_calendars"]["cn"];
    assert_eq!((cn["time_zone"].as_str(), cn["dates"].as_array().map(Vec::len)), (Some("Asia/Shanghai"), Some(33)));
    let (_, presets) = gw.api("GET", "/provider-presets", None).await;
    let deepseek = presets.as_array().unwrap().iter().find(|p| p["name"] == "deepseek").unwrap();
    assert_eq!(deepseek["holidays"], "cn");

    let mut bad = settings.clone();
    bad["holiday_calendars"]["cn"]["time_zone"] = json!("Beijing");
    let (st, v) = gw.api("PUT", "/settings", Some(bad)).await;
    assert_eq!(st, 422, "{v}");
    let mut bad = settings.clone();
    bad["holiday_calendars"]["CN!"] = cn.clone();
    assert_eq!(gw.api("PUT", "/settings", Some(bad)).await.0, 422);

    // A provider names a calendar the settings have.
    let provider = |holidays: &str| {
        json!({ "name": format!("ds-{holidays}"), "family": "openai",
        "base_url": "https://api.deepseek.com", "holidays": holidays })
    };
    let (st, v) = gw.api("POST", "/providers", Some(provider("jp"))).await;
    assert_eq!(st, 422, "{v}");
    let (st, v) = gw.api("POST", "/providers", Some(provider("cn"))).await;
    assert_eq!(st, 201, "{v}");
    assert_eq!(v["holidays"], "cn");
}

#[tokio::test]
async fn a_discovery_run_left_open_is_closed_and_runs_go_on() {
    let gw = TestGw::start(None).await;
    // A run that its instance left open, as when it stopped halfway. It used to keep every later
    // run from starting, asked for or scheduled.
    let store = pacod_gw_core::store::Store::connect(&gw.database_url, 2).await.unwrap();
    store.job_start_run("discovery", "run_left_open", "schedule", None).await.unwrap();

    let next = next_run(&gw, &["run_left_open"]).await;
    assert_eq!(next["trigger"], "manual", "{next}");
    assert_eq!(next["ok"], true, "{next}");
    let (_, runs) = gw.api("GET", "/discovery/runs", None).await;
    let left = runs.as_array().unwrap().iter().find(|r| r["id"] == "run_left_open").expect("listed").clone();
    assert!(left["finished_at"].is_string(), "{left}");
    assert_eq!(left["ok"], false, "{left}");
    assert!(left["message"].as_str().unwrap().starts_with("did not finish"), "{left}");
}

#[tokio::test]
async fn a_discovery_run_that_breaks_off_is_closed() {
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(None).await;
    let deepseek = gw
        .create("/providers", json!({ "name": "deepseek", "family": "openai", "base_url": format!("{}/emulate/deepseek", mock.base_url()) }))
        .await
        .id;
    gw.create(&format!("/providers/{deepseek}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;

    // The proposals cannot be written: the run breaks off at its first one, and is closed as
    // failed rather than left open.
    let pool = sqlx::PgPool::connect(&gw.database_url).await.unwrap();
    sqlx::query("ALTER TABLE discovery_proposals RENAME TO discovery_proposals_away").execute(&pool).await.unwrap();
    let broken = next_run(&gw, &[]).await;
    assert_eq!(broken["ok"], false, "{broken}");
    assert!(broken["message"].as_str().unwrap().starts_with("the run broke off"), "{broken}");

    // Runs go on once the database works again.
    sqlx::query("ALTER TABLE discovery_proposals_away RENAME TO discovery_proposals").execute(&pool).await.unwrap();
    let next = next_run(&gw, &[broken["id"].as_str().unwrap()]).await;
    assert_eq!(next["ok"], true, "{next}");
    assert_eq!(next["result"]["deepseek"]["new"], 2, "{next}");
}
