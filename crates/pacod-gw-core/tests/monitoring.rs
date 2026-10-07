//! The monitoring API and the gauges behind the alert rules (concept sections 14.7 and 14.9).

mod common;

use std::time::Duration;

use common::TestGw;
use pacod_gw_testkit::mock::{Behavior, MockProvider};
use serde_json::{Value, json};

#[tokio::test]
async fn live_figures_health_and_open_alerts() {
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    gw.priced_model(
        &fx,
        "openai/gpt-x",
        "gpt-x",
        json!({}),
        json!({ "currency": "USD", "input": 2.0, "output": 10.0 }),
    )
    .await;
    gw.model(&fx, "openai/flaky", "flaky", json!({ "free": true })).await;
    let limits = format!("/orgs/{}/workspaces/{}/limits", fx.org, fx.ws);
    gw.create(&limits, json!({ "name": "daily-requests", "subject": "workspace", "metric": "requests", "window": "day", "amount": 10.0, "alert_thresholds": [50, 100] })).await;
    gw.create(&format!("/orgs/{}/limits", fx.org), json!({ "name": "org-spend", "subject": "organisation", "metric": "spend", "window": "month", "amount": 1000.0 })).await;

    let call = |model: &'static str| {
        let (http, url, key) = (gw.http.clone(), format!("{}/v1/chat/completions", gw.data), fx.key.clone());
        async move {
            http.post(url)
                .bearer_auth(key)
                .json(&json!({ "model": model, "messages": [{ "role": "user", "content": "hi" }] }))
                .send()
                .await
                .unwrap()
                .status()
                .as_u16()
        }
    };
    // Nothing is open at first.
    let (st, alerts) = gw.api("GET", "/monitoring/alerts", None).await;
    assert_eq!((st, alerts.as_array().unwrap().len()), (200, 0), "{alerts}");

    for _ in 0..5 {
        assert_eq!(call("openai/gpt-x").await, 200);
    }
    // A model whose provider keeps failing: both attempts of each call.
    mock.set_script(Behavior::parse_all(["fail=500x30"]).unwrap());
    for _ in 0..5 {
        assert_eq!(call("openai/flaky").await, 502);
    }
    mock.set_script([]);

    let (st, live) = gw.api("GET", "/monitoring/live", None).await;
    assert_eq!(st, 200, "{live}");
    assert_eq!(live["in_flight"], 0);
    assert!(live["requests_per_second"].as_f64().unwrap() > 0.0 && live["error_ratio"].as_f64().unwrap() > 0.0);
    assert!(live["gateway_delay_p50_ms"].is_number());
    assert_eq!((live["config_lag_versions"].as_i64(), live["events_dropped"].as_u64()), (Some(0), Some(0)));

    let (_, health) = gw.api("GET", "/monitoring/health", None).await;
    let worst = &health[0];
    assert_eq!(
        (worst["model"].as_str(), worst["calls"].as_u64(), worst["success_ratio"].as_f64()),
        (Some("openai/flaky"), Some(5), Some(0.0))
    );
    let good = health.as_array().unwrap().iter().find(|h| h["model"] == "openai/gpt-x").unwrap();
    assert_eq!((good["failed"].as_u64(), good["success_ratio"].as_f64()), (Some(0), Some(1.0)));
    assert!(good["first_byte_ms"].is_number());

    // The limit is fully used (ten calls were counted), the provider of one model is down.
    let (_, alerts) = gw.api("GET", "/monitoring/alerts", None).await;
    let list = alerts.as_array().unwrap();
    let limit = list.iter().find(|a| a["kind"] == "limit").unwrap_or_else(|| panic!("no limit alert in {alerts}"));
    assert_eq!((limit["severity"].as_str(), limit["workspace_id"].as_str()), (Some("critical"), Some(fx.ws.as_str())));
    assert!(
        limit["title"].as_str().unwrap().contains("daily-requests")
            && limit["title"].as_str().unwrap().contains("100 %")
    );
    let provider = list.iter().find(|a| a["kind"] == "provider").unwrap();
    assert_eq!(provider["severity"], "critical");
    assert!(provider["title"].as_str().unwrap().contains("5 of 5"));
    assert_eq!(list[0]["severity"], "critical", "most severe first");
    assert!(
        !list
            .iter()
            .any(|a| a["id"].as_str().unwrap().contains("org-spend")
                || a["title"].as_str().unwrap().contains("org-spend")),
        "a limit far from its threshold is not an alert"
    );

    // Someone who can only read the workspace sees its limit alerts and nothing about the platform.
    let user = gw.create("/users", json!({ "email": "viewer@example.test", "password": "a long password 1" })).await.id;
    gw.create(
        "/role-bindings",
        json!({ "principal": user, "role": "viewer", "scope": { "type": "workspace", "id": fx.ws } }),
    )
    .await;
    let token = gw.login("viewer@example.test", "a long password 1").await;
    let (st, own) = gw.api_as(&token, "GET", "/monitoring/alerts", None).await;
    assert_eq!(st, 200);
    assert!(own.as_array().unwrap().iter().all(|a| a["kind"] == "limit") && !own.as_array().unwrap().is_empty());
    assert_eq!(gw.api_as(&token, "GET", "/monitoring/live", None).await.0, 403);

    // The gauges the alert rules read are exported.
    let metrics_url = format!("http://{}/metrics", gw.app.metrics_addr);
    let mut text = String::new();
    for _ in 0..40 {
        text = gw.http.get(&metrics_url).send().await.unwrap().text().await.unwrap();
        // The gauges are refreshed every second in tests: wait for the final count.
        if text.contains("gw_limit_used_ratio{organisation=\"acme\",workspace=\"prod\",limit=\"daily-requests\"} 1\n")
            && text.contains("gw_upstream_health")
            && text.contains("gw_price_age_days")
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let value = |prefix: &str| -> Option<f64> {
        text.lines().find(|l| l.starts_with(prefix)).and_then(|l| l.rsplit(' ').next()).and_then(|v| v.parse().ok())
    };
    assert_eq!(
        value("gw_limit_used_ratio{organisation=\"acme\",workspace=\"prod\",limit=\"daily-requests\"}"),
        Some(1.0),
        "{}",
        text.lines().filter(|l| l.starts_with("gw_limit")).collect::<Vec<_>>().join("\n")
    );
    assert_eq!(value("gw_upstream_health{provider=\"openai\",model=\"openai/flaky\"}"), Some(0.0));
    assert!(value("gw_price_age_days{model=\"openai/gpt-x\"}").is_some_and(|d| (0.0..0.01).contains(&d)));
    let _: Value = live;
}
