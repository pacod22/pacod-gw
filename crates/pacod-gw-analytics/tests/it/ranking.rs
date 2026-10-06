//! `GET /reports/ranking` against a data set of its own: the fixture of `support` has no timing,
//! streams or paths, which are what the ranking compares.

use pacod_gw_events::UsageEvent;
use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::support::{TestApp, at, id};

const RANGE: &str = "from=2026-03-12T00:00:00Z&to=2026-03-13T00:00:00Z";

/// One call. Everything not set here is a successful, priced, unstreamed chat call.
struct Call {
    provider: &'static str,
    model: &'static str,
    path: &'static str,
    status: &'static str,
    http_status: u16,
    attempts: u8,
    first_byte_ms: f32,
    stream_ms: f32,
    output: u64,
    cost: Option<f64>,
    currency: &'static str,
}

const OK: Call = Call {
    provider: "",
    model: "",
    path: "/v1/chat/completions",
    status: "ok",
    http_status: 200,
    attempts: 1,
    first_byte_ms: 0.0,
    stream_ms: 0.0,
    output: 0,
    cost: None,
    currency: "USD",
};

fn event(number: u128, call: Call) -> UsageEvent {
    let mut event = UsageEvent::new(id(number), "i-1");
    event.timestamp = at("2026-03-12T10:00:00Z") + chrono::Duration::seconds(number as i64);
    event.organisation_id = "org-a".into();
    event.workspace_id = "ws-1".into();
    event.mode = "managed".into();
    event.interface = "openai".into();
    event.path = call.path.into();
    event.provider = call.provider.into();
    event.model_requested = call.model.into();
    event.model_served = call.model.into();
    event.status = call.status.into();
    event.http_status = call.http_status;
    event.attempts = call.attempts;
    event.stream = call.stream_ms > 0.0;
    event.timing.upstream_first_byte_ms = call.first_byte_ms;
    event.timing.upstream_stream_ms = call.stream_ms;
    event.usage.input_tokens = 1000 - call.output;
    event.usage.output_tokens = call.output;
    event.usage_source = "provider".into();
    event.cost = call.cost;
    event.price_missing = call.cost.is_none();
    event.currency = call.currency.into();
    event
}

/// ```text
///  provider/model     calls                                                   ranked with min_requests=2
///  anthropic/fast     4 ok: first byte 100,100,200,200; streamed 10 ms/token;  yes
///                     0.002 per 1000 tokens. Plus one rejected, one cancelled
///                     and one token count, none of which count.
///  openai/cheap       3 ok (first byte 300, 0.001 per 1000 tokens), one 429    yes
///                     without a price
///  google/mixed       4 ok, first byte 150; priced in USD and EUR              yes, cost unranked
///  small/tiny         1 ok                                                     no
///  openai/voice       1 ok speech call                                         only with kind=speech
/// ```
fn data() -> Vec<UsageEvent> {
    let mut events = Vec::new();
    let mut add = |call: Call| events.push(event(events.len() as u128 + 1, call));
    for first_byte_ms in [100.0, 100.0, 200.0, 200.0] {
        add(Call {
            provider: "anthropic",
            model: "fast",
            first_byte_ms,
            stream_ms: 1000.0,
            output: 100,
            cost: Some(0.002),
            ..OK
        });
    }
    add(Call { provider: "anthropic", model: "fast", status: "rejected", http_status: 429, attempts: 0, ..OK });
    add(Call { provider: "anthropic", model: "fast", status: "cancelled", http_status: 499, ..OK });
    add(Call { provider: "anthropic", model: "fast", path: "/v1/messages/count_tokens", ..OK });
    for _ in 0..3 {
        add(Call { provider: "openai", model: "cheap", first_byte_ms: 300.0, cost: Some(0.001), ..OK });
    }
    add(Call { provider: "openai", model: "cheap", status: "error", http_status: 429, ..OK });
    for currency in ["USD", "USD", "EUR", "EUR"] {
        add(Call { provider: "google", model: "mixed", first_byte_ms: 150.0, cost: Some(0.001), currency, ..OK });
    }
    add(Call { provider: "small", model: "tiny", first_byte_ms: 50.0, cost: Some(0.0001), ..OK });
    add(Call {
        provider: "openai",
        model: "voice",
        path: "/v1/audio/speech",
        first_byte_ms: 80.0,
        cost: Some(0.015),
        ..OK
    });
    events
}

async fn app() -> TestApp {
    let app = TestApp::start().await;
    app.store_usage("ranking-1", &data()).await;
    app
}

fn names(report: &Value) -> Vec<String> {
    report["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| match row["model"].as_str() {
            Some(model) => format!("{}/{model}", row["provider"].as_str().expect("provider")),
            None => row["provider"].as_str().expect("provider").to_owned(),
        })
        .collect()
}

fn row<'a>(report: &'a Value, model: &str) -> &'a Value {
    report["rows"].as_array().expect("rows").iter().find(|row| row["model"] == model).expect("the model is ranked")
}

fn close(value: &Value, expected: f64) {
    let actual = value.as_f64().unwrap_or_else(|| panic!("a number, not {value}"));
    assert!((actual - expected).abs() < 1e-9, "{actual} is not {expected}");
}

#[tokio::test]
async fn models_are_ranked_on_their_metrics_and_the_preset_combines_them() {
    let app = app().await;
    let report = app.json(&format!("/reports/ranking?{RANGE}&min_requests=2")).await;

    assert_eq!(
        (&report["kind"], &report["by"], &report["preset"], &report["min_requests"], &report["currency"]),
        (&json!("chat"), &json!("model"), &json!("balanced"), &json!(2), &json!("USD"))
    );
    assert_eq!(
        report["weights"],
        json!({ "success_rate": 0.3, "first_byte_p50_ms": 0.15, "first_byte_p95_ms": 0.15,
                "output_ms_per_token": 0.15, "cost": 0.25 })
    );
    assert_eq!(names(&report), ["google/mixed", "anthropic/fast", "openai/cheap", "small/tiny"]);

    // mixed: best success and first byte; no stream, no comparable cost → 60 / 0.6.
    let mixed = row(&report, "mixed");
    assert_eq!((&mixed["rank"], &mixed["score"], &mixed["ranked"]), (&json!(1), &json!(100.0), &json!(true)));
    assert_eq!(mixed["metrics"]["cost"], json!({ "value": null, "rank": null, "score": null }));
    assert_eq!(mixed["info"]["mixed_currencies"], json!(true));

    // fast: the rejected and cancelled calls and the token count do not count.
    let fast = row(&report, "fast");
    assert_eq!((&fast["requests"], &fast["eligible"], &fast["score"]), (&json!(6), &json!(4), &json!(60.0)));
    assert_eq!(fast["metrics"]["success_rate"], json!({ "value": 1.0, "rank": 1, "score": 100.0 }));
    assert_eq!(fast["metrics"]["first_byte_p50_ms"], json!({ "value": 200.0, "rank": 2, "score": 50.0 }));
    assert_eq!(fast["metrics"]["output_ms_per_token"], json!({ "value": 10.0, "rank": 1, "score": 100.0 }));
    assert_eq!(fast["metrics"]["cost"]["rank"], json!(2));
    close(&fast["metrics"]["cost"]["value"], 2.0);
    close(&fast["info"]["output_tokens_per_second"], 100.0);

    // cheap: the unpriced 429 counts against success, not as a free call.
    let cheap = row(&report, "cheap");
    assert_eq!(cheap["metrics"]["success_rate"], json!({ "value": 0.75, "rank": 3, "score": 0.0 }));
    assert_eq!(cheap["metrics"]["cost"]["rank"], json!(1));
    close(&cheap["metrics"]["cost"]["value"], 1.0);
    assert_eq!((&cheap["info"]["rate_limited_share"], &cheap["info"]["price_coverage"]), (&json!(0.25), &json!(0.75)));
    assert_eq!(cheap["score"], json!(29.41));

    // tiny: too few calls, listed last without ranks.
    let tiny = row(&report, "tiny");
    assert_eq!((&tiny["rank"], &tiny["score"], &tiny["ranked"]), (&Value::Null, &Value::Null, &json!(false)));
    assert_eq!(tiny["metrics"]["first_byte_p50_ms"], json!({ "value": 50.0, "rank": null, "score": null }));
}

#[tokio::test]
async fn the_preset_changes_the_order() {
    let app = app().await;
    let cost = app.json(&format!("/reports/ranking?{RANGE}&min_requests=2&preset=cost")).await;
    // mixed: 30 of 0.30; cheap: 65 of 0.95; fast: 20 + 2.5 + 2.5 + 5 of 1.
    assert_eq!(names(&cost)[..3], ["google/mixed", "openai/cheap", "anthropic/fast"]);
    assert_eq!(row(&cost, "cheap")["score"], json!(68.42));
}

#[tokio::test]
async fn providers_can_be_ranked_instead_of_models() {
    let app = app().await;
    let report = app.json(&format!("/reports/ranking?{RANGE}&min_requests=2&by=provider")).await;
    assert_eq!(report["by"], json!("provider"));
    assert_eq!(names(&report), ["google", "anthropic", "openai", "small"]);
    let openai = &report["rows"][2];
    // The speech call is not chat.
    assert_eq!((&openai["model"], &openai["eligible"]), (&Value::Null, &json!(4)));
}

#[tokio::test]
async fn speech_is_ranked_apart_from_chat() {
    let app = app().await;
    let report = app.json(&format!("/reports/ranking?{RANGE}&min_requests=1&kind=speech")).await;
    assert_eq!(names(&report), ["openai/voice"]);
    let voice = &report["rows"][0];
    close(&voice["metrics"]["cost"]["value"], 0.015);
    assert_eq!(voice["metrics"]["output_ms_per_token"]["value"], Value::Null);
    assert_eq!(voice["score"], json!(100.0));
}

#[tokio::test]
async fn relayed_calls_count_by_the_ending_of_their_path() {
    let app = TestApp::start().await;
    let paths = [
        ("chat", "/v1beta/models/gemini-x:generateContent"),
        ("chat", "/v1beta/models/gemini-x:streamGenerateContent"),
        ("chat", "/api/chat"),
        ("chat", "/api/generate"),
        ("chat", "/v1/responses"),
        ("chat", "/openai/v1/chat/completions"),
        ("speech", "/v1/audio/speech"),
        ("none", "/v1beta/models/gemini-x:countTokens"),
        ("none", "/v1/embeddings"),
        ("none", "/api/tags"),
    ];
    let events: Vec<UsageEvent> = paths
        .iter()
        .enumerate()
        .map(|(index, (_, path))| event(index as u128 + 1, Call { provider: "relay", model: path, path, ..OK }))
        .collect();
    app.store_usage("ranking-relay", &events).await;

    for kind in ["chat", "speech"] {
        let report = app.json(&format!("/reports/ranking?{RANGE}&min_requests=1&kind={kind}")).await;
        let mut models: Vec<&str> =
            report["rows"].as_array().expect("rows").iter().map(|row| row["model"].as_str().expect("model")).collect();
        models.sort_unstable();
        let mut expected: Vec<&str> = paths.iter().filter(|(k, _)| *k == kind).map(|(_, path)| *path).collect();
        expected.sort_unstable();
        assert_eq!(models, expected, "{kind}");
    }
}

#[tokio::test]
async fn filters_and_csv_apply() {
    let app = app().await;
    let report = app.json(&format!("/reports/ranking?{RANGE}&min_requests=2&provider=openai")).await;
    assert_eq!(names(&report), ["openai/cheap"]);
    // Another organisation sees nothing.
    let other = app.json(&format!("/reports/ranking?{RANGE}&organisation_id=org-b")).await;
    assert_eq!(other["rows"], json!([]));

    let response = app.get(&format!("/reports/ranking?{RANGE}&min_requests=2&provider=openai&format=csv")).await;
    assert_eq!(response.status(), StatusCode::OK);
    let text = response.text().await.expect("body");
    let mut lines = text.lines();
    assert!(
        lines.next().expect("header").starts_with("rank,score,ranked,provider,model,requests,eligible,success_rate,")
    );
    assert!(lines.next().expect("row").starts_with("1,100.0,true,openai,cheap,4,4,0.75,1,"));
}

#[tokio::test]
async fn invalid_parameters_are_refused() {
    let app = app().await;
    for (query, parameter) in [
        ("preset=fastest", "preset"),
        ("kind=embedding", "kind"),
        ("by=template", "by"),
        ("min_requests=0", "min_requests"),
        ("min_requests=lots", "min_requests"),
    ] {
        let error = app.error(&format!("/reports/ranking?{RANGE}&{query}"), StatusCode::BAD_REQUEST).await;
        assert_eq!(error["code"], json!("invalid_parameter"), "{query}");
        assert!(error["message"].as_str().expect("message").contains(parameter), "{query}: {error}");
    }
    let error = app.error(&format!("/reports/ranking?{RANGE}&limit=5"), StatusCode::BAD_REQUEST).await;
    assert_eq!(error["code"], json!("unknown_parameter"));
}
