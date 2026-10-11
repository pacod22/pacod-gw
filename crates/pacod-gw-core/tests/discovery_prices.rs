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
