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
            json!({ "name": "vertex", "family": "gemini", "managed_api": "native", "platform": "vertex",
                    "auth": "google_service_account", "base_url": base }),
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
async fn proxy_mode_relays_only_the_projects_model_methods() {
    let w = setup(None).await;
    let base = "/emulate/vertex/v1/projects/p/locations/europe-west4/publishers/google/models/gemini-3.8-flash";
    let body = json!({ "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }] });
    let call = |method: reqwest::Method, path: String| {
        w.gw.http
            .request(method, format!("{}/proxy/vertex{path}", w.gw.data))
            .bearer_auth(&w.fx.key)
            .header("x-goog-user-project", "someone-elses-project")
            .header("x-http-method-override", "DELETE")
            .json(&body)
            .send()
    };
    let r = call(reqwest::Method::POST, format!("{base}:generateContent")).await.unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    let sent = w.mock.captured().into_iter().rev().find(|c| c.path.ends_with(":generateContent")).unwrap();
    assert!(sent.header("authorization").is_some_and(|a| a.starts_with("Bearer mock-access-")), "the minted token");
    assert!(sent.header("x-goog-user-project").is_none() && sent.header("x-http-method-override").is_none());
    let reached = w.mock.captured().len();
    for (method, path) in [
        (reqwest::Method::GET, format!("{base}:generateContent")),
        (reqwest::Method::POST, base.replace("projects/p/", "projects/other/") + ":generateContent"),
        (reqwest::Method::POST, format!("{base}:deleteModel")),
        (reqwest::Method::POST, format!("{base}:generateContent/../../../../datasets")),
        (reqwest::Method::POST, base.replace("models/", "models/x%2F..%2F") + ":generateContent"),
        (reqwest::Method::POST, format!("{base}:generateContent?$alt=json")),
        (reqwest::Method::POST, "/emulate/vertex/v1/projects/p/locations/europe-west4/datasets".into()),
    ] {
        let r = call(method.clone(), path.clone()).await.unwrap();
        assert!(r.status().is_client_error(), "{method} {path}: {}", r.status());
    }
    assert_eq!(w.mock.captured().len(), reached, "nothing refused reached the provider");
    // The project's files and data stores only on a credential of the caller's own.
    for denied in [
        json!({ "contents": [{ "role": "user", "parts": [{ "fileData": { "fileUri": "gs://bucket/x.pdf", "mimeType": "application/pdf" } }] }] }),
        json!({ "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }], "tools": [{ "retrieval": { "vertexAiSearch": { "datastore": "d" } } }] }),
    ] {
        let r =
            w.gw.http
                .post(format!("{}/proxy/vertex{base}:generateContent", w.gw.data))
                .bearer_auth(&w.fx.key)
                .json(&denied)
                .send()
                .await
                .unwrap();
        assert_eq!(r.status(), 403);
    }
}

#[tokio::test]
async fn vertex_signatures_go_back_to_vertex_only() {
    let w = setup(None).await;
    let tools = json!([{ "type": "function", "function": { "name": "lookup", "parameters": { "type": "object" } } }]);
    let user = json!({ "role": "user", "content": "find it" });
    w.mock.set_script(Behavior::parse_all(["tool=lookup"]).unwrap());
    let post = |body: Value| {
        w.gw.http.post(format!("{}/v1/chat/completions", w.gw.data)).bearer_auth(&w.fx.key).json(&body).send()
    };
    let first: Value = post(json!({ "model": "vertex/flash", "tools": tools, "messages": [user] }))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let message = first["choices"][0]["message"].clone();
    let sig = message["tool_calls"][0]["extra_content"]["google"]["thought_signature"].as_str().unwrap().to_owned();
    assert!(sig.starts_with("gw1.vertex."), "labelled with its origin: {sig}");
    let result = json!({ "role": "tool", "tool_call_id": message["tool_calls"][0]["id"], "content": "42" });
    let r =
        post(json!({ "model": "vertex/flash", "tools": tools, "messages": [user, message, result] })).await.unwrap();
    assert_eq!(r.status(), 200, "the emulation refuses a call without its signature: {}", r.text().await.unwrap());
    let sent =
        w.mock.captured().into_iter().rev().find(|c| c.path.ends_with(":generateContent")).unwrap().json().unwrap();
    assert_eq!(sent["contents"][1]["parts"][0]["thoughtSignature"], sig.trim_start_matches("gw1.vertex."));
    // The same turn with an AI Studio signature instead: not Vertex's, so its placeholder goes.
    let mut foreign = message.clone();
    foreign["tool_calls"][0]["extra_content"]["google"]["thought_signature"] = json!("QUlTdHVkaW8=");
    let r =
        post(json!({ "model": "vertex/flash", "tools": tools, "messages": [user, foreign, result] })).await.unwrap();
    assert_eq!(r.status(), 200);
    let sent =
        w.mock.captured().into_iter().rev().find(|c| c.path.ends_with(":generateContent")).unwrap().json().unwrap();
    assert_eq!(sent["contents"][1]["parts"][0]["thoughtSignature"], "skip_thought_signature_validator");
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
    // The scheme cannot change while the provider holds the other kind of credential: the key
    // file would go out as an API key.
    let (_, current) = w.gw.api("GET", &format!("/providers/{}", w.provider), None).await;
    let mut keyed = current.clone();
    keyed["auth"] = json!("x_goog_api_key");
    let (st, v) = w.gw.api("PUT", &format!("/providers/{}", w.provider), Some(keyed)).await;
    assert_eq!(st, 422, "{v}");
    // The scheme belongs to Gemini.
    let (st, _) = w
        .gw
        .api("POST", "/providers", Some(json!({ "name": "openai-sa", "family": "openai", "auth": "google_service_account", "base_url": "https://api.openai.com/v1" })))
        .await;
    assert_eq!(st, 422);
}

#[tokio::test]
async fn express_mode_relays_with_the_providers_key_and_never_the_callers_google_headers() {
    let w = setup(None).await;
    let base = format!("{}/emulate/vertex/v1/publishers/google", w.mock.base_url());
    let express =
        w.gw.create(
            "/providers",
            json!({ "name": "vertex-express", "family": "gemini", "managed_api": "native", "platform": "vertex",
                    "auth": "x_goog_api_key", "base_url": base }),
        )
        .await
        .id;
    w.gw.create(&format!("/providers/{express}/credentials"), json!({ "secret": "AIza-express-key-of-the-provider" }))
        .await;
    let id = w.gw.draft_model(&express, "vertex-express/flash", "gemini-3.8-flash", json!({ "free": true })).await;
    w.gw.activate(&id).await;
    w.gw.create(&format!("/orgs/{}/grants", w.fx.org), json!({ "model_id": id })).await;
    let r =
        w.gw.http
            .post(format!(
                "{}/proxy/vertex-express/emulate/vertex/v1/publishers/google/models/gemini-3.8-flash:generateContent",
                w.gw.data
            ))
            .bearer_auth(&w.fx.key)
            .header("x-goog-user-project", "someone-elses-project")
            .json(&json!({ "contents": [{ "role": "user", "parts": [{ "text": "hi" }] }] }))
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    let sent = w.mock.captured().into_iter().rev().find(|c| c.path.ends_with(":generateContent")).unwrap();
    assert_eq!(sent.header("x-goog-api-key"), Some("AIza-express-key-of-the-provider"));
    assert!(sent.header("x-goog-user-project").is_none() && sent.header("authorization").is_none());
}
