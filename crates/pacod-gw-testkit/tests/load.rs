//! `gw-load`'s series against the mock alone: every call made and counted, as before it ran
//! on the load agents' engine.

use pacod_gw_testkit::load::{Plan, run};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::json;

#[tokio::test]
async fn a_series_makes_and_counts_every_call() {
    let mock = MockProvider::spawn().await;
    let http = reqwest::Client::new();
    let plan = Plan {
        url: format!("{}/v1/chat/completions", mock.base_url()),
        key: None,
        body: json!({ "model": "gpt-mock", "stream": true, "messages": [{ "role": "user", "content": "word word word" }] }),
        requests: 120,
        concurrency: 8,
    };
    let o = run(&http, &plan).await;
    assert_eq!((o.ok, o.failed), (120, 0));
    assert_eq!(mock.captured().len(), 120);
    assert!(o.total_ms.p50 >= o.first_byte_ms.p50 && o.total_ms.max > 0.0, "{o:?}");
    assert!(o.gateway_delay_ms.is_none(), "the mock sends no x-gw-delay-ms");

    let failing = Plan { body: json!({ "model": "gpt-mock" }), requests: 5, ..plan };
    let o = run(&http, &failing).await;
    assert_eq!((o.ok, o.failed), (0, 5), "a 400 counts as failed");
}
