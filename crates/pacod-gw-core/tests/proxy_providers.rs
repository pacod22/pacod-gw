//! Proxy mode for Gemini's and Ollama's own APIs (concept sections 4.2 and 8.1): the key in
//! Google's `?key=`, the error envelopes those SDKs read, usage from a Gemini stream without
//! `alt=sse`, Gemini's objects on the shared credential, unknown Gemini tools, and the probe of
//! an Ollama model.

mod common;

use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

/// A gateway with a Gemini provider (`gemini`, native API at the mock), a second one whose
/// managed and proxy base is the mock's emulation of the compatible endpoint (`gemini-compat`),
/// and an Ollama provider (`ollama`).
struct World {
    gw: TestGw,
    mock: MockProvider,
    sink: IngestSink,
    fx: Fixture,
}

async fn setup() -> World {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("gemini", &format!("{}/v1beta", mock.base_url())).await;
    let base = mock.base_url();
    let compat = gw
        .create(
            "/providers",
            json!({ "name": "gemini-compat", "family": "gemini",
                    "base_url": format!("{base}/emulate/gemini-openai/v1beta/openai"),
                    "proxy_base_url": format!("{base}/emulate/gemini-openai") }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{compat}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
    let m = gw.draft_model(&compat, "gemini-compat/flash", "gemini-3.8-flash", json!({ "free": true })).await;
    gw.activate(&m).await;
    gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": m })).await;
    let ollama = gw
        .create("/providers", json!({ "name": "ollama", "family": "ollama", "base_url": format!("{base}/v1") }))
        .await
        .id;
    let q = gw.draft_model(&ollama, "ollama/qwen", "qwen3", json!({ "free": true })).await;
    gw.activate(&q).await;
    gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": q })).await;
    World { gw, mock, sink, fx }
}

fn hello() -> Value {
    json!({ "contents": [{ "role": "user", "parts": [{ "text": "hello" }] }] })
}

#[tokio::test]
async fn a_gateway_key_in_the_query_authenticates_and_goes_no_further() {
    let w = setup().await;
    w.gw.api(
        "PUT",
        &format!("/orgs/{}/workspaces/{}", w.fx.org, w.fx.ws),
        Some(json!({ "name": "prod", "tag_keys": ["purpose", "site"], "settings": { "audit": { "mode": "on" } } })),
    )
    .await;
    let url = format!("{}/proxy/gemini/v1beta/models/m:streamGenerateContent?alt=sse&key={}", w.gw.data, w.fx.key);
    let r = w.gw.http.post(url).json(&hello()).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.text().await.unwrap().contains("data: "));

    let c = &w.mock.captured()[0];
    assert_eq!(c.path, "/v1beta/models/m:streamGenerateContent?alt=sse", "the key is not relayed");
    assert_eq!(c.header("x-goog-api-key"), Some(PROVIDER_SECRET));
    assert!(c.headers.iter().all(|(_, v)| !v.contains(&w.fx.key)));
    let record = w.sink.wait_audit(1).await.remove(0);
    let text = serde_json::to_string(&record).unwrap();
    assert!(!text.contains(&w.fx.key), "the audit record holds no key");
    assert!(record.client_request.url.ends_with("streamGenerateContent?alt=sse"), "{}", record.client_request.url);

    // A key of the provider's own in the query is not the gateway's: it stays, and the call
    // needs a gateway key elsewhere.
    let r =
        w.gw.http
            .post(format!("{}/proxy/gemini/v1beta/models/m:generateContent?key=AIzaNotOurs", w.gw.data))
            .json(&hello())
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 401);
}

#[tokio::test]
async fn errors_come_in_the_envelope_of_the_api_called() {
    let w = setup().await;
    let post = |path: &str, body: Value| {
        w.gw.http
            .post(format!("{}/proxy/{path}", w.gw.data))
            .header("x-goog-api-key", "pgw_nope_nope")
            .json(&body)
            .send()
    };
    let r = post("gemini/v1beta/models/m:generateContent", hello()).await.unwrap();
    assert_eq!(r.status(), 401);
    let v: Value = r.json().await.unwrap();
    assert_eq!((v["error"]["code"].as_u64(), v["error"]["status"].as_str()), (Some(401), Some("UNAUTHENTICATED")));
    let r = post("gemini-compat/v1beta/openai/chat/completions", json!({})).await.unwrap();
    let v: Value = r.json().await.unwrap();
    assert!(v["error"]["type"].is_string(), "OpenAI's envelope on the compatible path: {v}");
    let r = post("ollama/api/chat", json!({})).await.unwrap();
    let v: Value = r.json().await.unwrap();
    assert!(v["error"].is_string(), "Ollama's native API: a bare message: {v}");
}

#[tokio::test]
async fn the_compatible_endpoint_gets_a_bearer_token_in_proxy_mode() {
    let w = setup().await;
    let r =
        w.gw.http
            .post(format!("{}/proxy/gemini-compat/v1beta/openai/chat/completions", w.gw.data))
            .bearer_auth(&w.fx.key)
            .json(&json!({ "model": "gemini-3.8-flash", "messages": [{ "role": "user", "content": "hi" }] }))
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 200);
    let c = &w.mock.captured()[0];
    assert_eq!(c.path, "/emulate/gemini-openai/v1beta/openai/chat/completions");
    assert_eq!(c.header("authorization"), Some(format!("Bearer {PROVIDER_SECRET}").as_str()));
    assert_eq!(c.header("x-goog-api-key"), None);
    // HTTP/1.1 still names the host: the client adds it (and on HTTP/2 only `:authority`).
    assert_eq!(c.header("host"), Some(w.mock.addr().to_string().as_str()));
}

#[tokio::test]
async fn a_gemini_stream_without_sse_is_counted() {
    let w = setup().await;
    let r =
        w.gw.http
            .post(format!("{}/proxy/gemini/v1beta/models/m:streamGenerateContent", w.gw.data))
            .header("x-goog-api-key", &w.fx.key)
            .header("x-mock", "thinking")
            .json(&hello())
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.text().await.unwrap().starts_with('['));
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!(e.usage_source, "provider");
    assert!(e.usage.input_tokens > 0 && e.usage.output_tokens > 0 && e.usage.reasoning_tokens > 0, "{:?}", e.usage);
}

#[tokio::test]
async fn gemini_objects_and_unknown_tools_are_checked() {
    let w = setup().await;
    let post = |body: Value| {
        w.gw.http
            .post(format!("{}/proxy/gemini/v1beta/models/m:generateContent", w.gw.data))
            .header("x-goog-api-key", &w.fx.key)
            .json(&body)
            .send()
    };
    let mut cached = hello();
    cached["cachedContent"] = json!("cachedContents/abc");
    let r = post(cached.clone()).await.unwrap();
    assert_eq!(r.status(), 403);
    assert_eq!(r.headers()["x-gw-error-type"], "own_credential_required");
    // With the workspace's own credential it reaches the workspace's own caches.
    let (st, v) =
        w.gw.api(
            "POST",
            &format!("/providers/{}/credentials", w.fx.provider),
            Some(json!({ "secret": "ws-own-secret", "scope": { "type": "workspace", "id": w.fx.ws } })),
        )
        .await;
    assert_eq!(st, 201, "{v}");
    w.gw.wait_applied().await;
    assert_eq!(post(cached).await.unwrap().status(), 200);

    // A workspace that restricts provider tools: a Gemini tool the gateway does not know is
    // refused by its key.
    w.gw.api(
        "PUT",
        &format!("/orgs/{}/workspaces/{}", w.fx.org, w.fx.ws),
        Some(json!({ "name": "prod", "tag_keys": ["purpose", "site"], "settings": { "provider_tools": ["web_search"] } })),
    )
    .await;
    let mut maps = hello();
    maps["tools"] = json!([{ "googleMaps": {} }]);
    let r = post(maps).await.unwrap();
    assert_eq!(r.status(), 403);
    let mut search = hello();
    search["tools"] = json!([{ "googleSearch": {} }]);
    assert_eq!(post(search).await.unwrap().status(), 200);
    assert_eq!(w.mock.captured().len(), 2);
}

#[tokio::test]
async fn the_probe_of_an_ollama_model_looks_for_it_in_the_list() {
    let w = setup().await;
    let (_, providers) = w.gw.api("GET", "/providers", None).await;
    let ollama = providers["items"].as_array().unwrap().iter().find(|p| p["name"] == "ollama").unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let listed = w.gw.draft_model(&ollama, "ollama/llama", "llama3.2", json!({ "free": true })).await;
    let (st, v) = w.gw.api("POST", &format!("/models/{listed}/probe"), None).await;
    assert_eq!((st, v["ok"].as_bool()), (200, Some(true)), "{v}");
    let missing = w.gw.draft_model(&ollama, "ollama/absent", "not-pulled", json!({ "free": true })).await;
    let (st, v) = w.gw.api("POST", &format!("/models/{missing}/probe"), None).await;
    assert_eq!((st, v["ok"].as_bool()), (200, Some(false)), "{v}");
    assert!(v["detail"].as_str().unwrap().contains("not-pulled"), "{v}");
}

#[tokio::test]
async fn a_template_governs_either_spelling_of_geminis_configuration() {
    // Google's JSON takes `generation_config` and snake_case members as well: a template's limit
    // must not be passed by spelling it the other way.
    let w = setup().await;
    w.gw.template(&w.fx, "capped", json!({ "model": "gemini/m", "max_output_tokens": 100 })).await;
    for config in [
        json!({ "generation_config": { "max_output_tokens": 99999, "temperature": 0.3 } }),
        json!({ "generationConfig": { "max_output_tokens": 99999 } }),
        json!({ "generationConfig": { "maxOutputTokens": 99999 }, "generation_config": { "max_output_tokens": 99999 } }),
    ] {
        let mut body = hello();
        body.as_object_mut().unwrap().extend(config.as_object().unwrap().clone());
        let r =
            w.gw.http
                .post(format!("{}/proxy/gemini/v1beta/models/@capped:generateContent", w.gw.data))
                .header("x-goog-api-key", &w.fx.key)
                .json(&body)
                .send()
                .await
                .unwrap();
        assert_eq!(r.status(), 200, "{config}: {}", r.text().await.unwrap());
        let sent = w.mock.captured().into_iter().rev().find(|c| c.path.ends_with("/models/m:generateContent")).unwrap();
        let v = sent.json().unwrap();
        assert_eq!(v["generationConfig"]["maxOutputTokens"], 100, "{config}: {v}");
        assert!(
            v.get("generation_config").is_none() && v["generationConfig"].get("max_output_tokens").is_none(),
            "{v}"
        );
    }
}
