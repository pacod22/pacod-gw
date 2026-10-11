//! Gemini on Vertex AI with a service account: the gateway mints access tokens from the key and
//! caches them, against the mock's token endpoint and its emulation of Vertex.

mod common;

use common::{Fixture, TestGw};
use pacod_gw_testkit::google::TEST_SERVICE_ACCOUNT;
use pacod_gw_testkit::mock::{Behavior, MockProvider};
use serde_json::{Value, json};

struct World {
    gw: TestGw,
    mock: MockProvider,
    fx: Fixture,
    provider: String,
}

/// A gateway whose token endpoint is the mock's (`expires_in` shortens the tokens), with a
/// Vertex provider on a service account and an active Gemini model granted to the fixture.
async fn setup(expires_in: Option<u64>) -> World {
    let mock = MockProvider::spawn().await;
    let token_url = format!(
        "{}/emulate/google-oauth/token{}",
        mock.base_url(),
        expires_in.map(|s| format!("?expires_in={s}")).unwrap_or_default()
    );
    let db = common::fresh_database().await;
    let gw = TestGw::start_on(&db, None, move |b| b.google_token_url = Some(token_url)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let base = format!("{}/emulate/vertex/v1/projects/p/locations/europe-west4/publishers/google", mock.base_url());
    let provider = gw
        .create(
            "/providers",
            json!({ "name": "vertex", "family": "gemini", "managed_api": "native", "auth": "google_service_account", "base_url": base }),
        )
        .await
        .id;
    let (st, v) = gw
        .api("POST", &format!("/providers/{provider}/credentials"), Some(json!({ "secret": TEST_SERVICE_ACCOUNT })))
        .await;
    assert_eq!(st, 201, "{v}");
    assert_eq!(
        v["fingerprint"],
        "pgw-test@pgw-test-project.iam.gserviceaccount.com (key ".to_owned() + &key_id()[..8] + ")"
    );
    let id = gw
        .draft_model(
            &provider,
            "vertex/flash",
            "gemini-3.8-flash",
            json!({ "free": true, "capability_preset": "gemini-3-flash" }),
        )
        .await;
    // The probe asks Vertex with a token too.
    let (st, v) = gw.api("POST", &format!("/models/{id}/transition"), Some(json!({ "to": "active" }))).await;
    assert_eq!(st, 200, "{v}");
    gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    World { gw, mock, fx, provider }
}

fn key_id() -> String {
    let v: Value = serde_json::from_str(TEST_SERVICE_ACCOUNT).unwrap();
    v["private_key_id"].as_str().unwrap().to_owned()
}

impl World {
    async fn chat(&self) -> (u16, Value) {
        let r = self
            .gw
            .http
            .post(format!("{}/v1/chat/completions", self.gw.data))
            .bearer_auth(&self.fx.key)
            .json(&json!({ "model": "vertex/flash", "messages": [{ "role": "user", "content": "hello" }] }))
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    async fn minted(&self) -> u64 {
        let v: Value = self
            .gw
            .http
            .get(format!("{}/__mock/google", self.mock.base_url()))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        v["minted"].as_u64().unwrap()
    }
}

#[tokio::test]
async fn one_token_serves_many_calls() {
    let w = setup(None).await;
    let calls = (0..20).map(|_| w.chat());
    for (st, v) in futures_util::future::join_all(calls).await {
        assert_eq!(st, 200, "{v}");
    }
    assert_eq!(w.minted().await, 1, "the probe's token served every call");
    // What Vertex received: the token, never the key.
    let sent = w
        .mock
        .captured()
        .into_iter()
        .rev()
        .find(|c| c.method == "POST" && c.path.contains(":generateContent"))
        .unwrap();
    assert!(sent.header("authorization").is_some_and(|a| a.starts_with("Bearer mock-access-")));
    assert!(sent.header("x-goog-api-key").is_none());
    let assertion = w.mock.captured().into_iter().find(|c| c.path.contains("google-oauth")).unwrap();
    assert!(!String::from_utf8_lossy(&assertion.body).contains("PRIVATE KEY"), "the key never leaves the gateway");
}

#[tokio::test]
async fn a_token_is_minted_again_before_it_runs_out() {
    // Tokens of 200 seconds are inside the five-minute margin from the start.
    let w = setup(Some(200)).await;
    let before = w.minted().await;
    for _ in 0..3 {
        assert_eq!(w.chat().await.0, 200);
    }
    assert_eq!(w.minted().await, before + 3);
}

#[tokio::test]
async fn a_refused_token_is_minted_again_and_the_call_sent_once_more() {
    let w = setup(None).await;
    let before = w.minted().await;
    w.mock.set_script(Behavior::parse_all(["status=401"]).unwrap());
    let (st, v) = w.chat().await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(w.minted().await, before + 1);
}

#[tokio::test]
async fn proxy_mode_does_not_relay_with_the_token() {
    let w = setup(None).await;
    let r = w
        .gw
        .http
        .post(format!("{}/proxy/vertex/v1/projects/p/locations/europe-west4/publishers/google/models/gemini-3.8-flash:generateContent", w.gw.data))
        .bearer_auth(&w.fx.key)
        .json(&json!({ "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }] }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
}

#[tokio::test]
async fn keys_and_service_accounts_are_not_mistaken() {
    let w = setup(None).await;
    let credentials = format!("/providers/{}/credentials", w.provider);
    // Not a service account's key, or a broken one.
    for secret in [
        json!("AIzaSyThisIsAnApiKey00000000000000000"),
        json!(TEST_SERVICE_ACCOUNT.replace("BEGIN PRIVATE KEY", "BEGIN RSA KEY")),
    ] {
        let (st, v) = w.gw.api("POST", &credentials, Some(json!({ "secret": secret }))).await;
        assert_eq!(st, 422, "{v}");
        assert!(!v.to_string().contains("PRIVATE KEY-----\\n"), "the answer never repeats the key: {v}");
    }
    // A service account's key on a provider that takes an API key.
    let (st, v) =
        w.gw.api(
            "POST",
            &format!("/providers/{}/credentials", w.fx.provider),
            Some(json!({ "secret": TEST_SERVICE_ACCOUNT })),
        )
        .await;
    assert_eq!(st, 422, "{v}");
    // The scheme belongs to Gemini.
    let (st, _) = w
        .gw
        .api("POST", "/providers", Some(json!({ "name": "openai-sa", "family": "openai", "auth": "google_service_account", "base_url": "https://api.openai.com/v1" })))
        .await;
    assert_eq!(st, 422);
}
