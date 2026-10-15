//! The load tests' latency profile (`tester/…/load/scene.py`): before each run the tester
//! rewrites an active mock provider's URLs to `…/x-mock/<options>`, and the options must then
//! reach the mock in managed mode (which forwards no `x-mock` header) and in proxy mode.

mod common;

use std::time::{Duration, Instant};

use common::TestGw;
use pacod_gw_testkit::mock::MockProvider;
use serde_json::json;

#[tokio::test]
async fn a_profile_in_an_active_providers_urls_reaches_the_mock_in_both_modes() {
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;

    // As the tester does it: read the provider, change its two URLs, write it back.
    let (_, mut provider) = gw.api("GET", &format!("/providers/{}", fx.provider), None).await;
    provider["base_url"] = json!(format!("{}/x-mock/delay=300,chunks=3/v1", mock.base_url()));
    provider["proxy_base_url"] = json!(format!("{}/x-mock/delay=300,chunks=3", mock.base_url()));
    let (st, v) = gw.api("PUT", &format!("/providers/{}", fx.provider), Some(provider)).await;
    assert_eq!(st, 200, "an active provider's URLs can change: {v}");
    gw.wait_applied().await;

    let body = json!({ "model": "openai/m", "messages": [{ "role": "user", "content": "one two three" }] });
    let started = Instant::now();
    let r =
        gw.http.post(format!("{}/v1/chat/completions", gw.data)).bearer_auth(&fx.key).json(&body).send().await.unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    assert!(started.elapsed() >= Duration::from_millis(300), "managed mode carried the delay");

    let upstream = json!({ "model": "m", "messages": [{ "role": "user", "content": "one two three" }] });
    let started = Instant::now();
    let r = gw
        .http
        .post(format!("{}/proxy/openai/v1/chat/completions", gw.data))
        .bearer_auth(&fx.key)
        .json(&upstream)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    assert!(started.elapsed() >= Duration::from_millis(300), "proxy mode carried the delay");

    let paths: Vec<String> = mock.captured().into_iter().map(|c| c.path).collect();
    assert!(paths.contains(&"/x-mock/delay=300,chunks=3/v1/chat/completions".to_owned()), "{paths:?}");
}
