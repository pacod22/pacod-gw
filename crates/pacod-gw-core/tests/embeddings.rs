//! Managed embeddings (`POST /v1/embeddings`) on OpenAI-shaped providers, Gemini's own API and
//! Vertex AI, and token counting for Gemini models through Anthropic's interface.

mod common;

use base64::Engine as _;
use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_testkit::google::TEST_SERVICE_ACCOUNT;
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

struct World {
    gw: TestGw,
    mock: MockProvider,
    sink: IngestSink,
    fx: Fixture,
}

/// The fixture's OpenAI provider with an embedding and a chat model, Gemini's own API with an
/// embedding and a chat model, and Vertex AI (service account) with one of each.
async fn setup() -> World {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let token_url = format!("{}/emulate/google-oauth/token", mock.base_url());
    let db = common::fresh_database().await;
    let gw = TestGw::start_on(&db, Some(&sink), move |b| b.google_token_url = Some(token_url)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let price = json!({ "currency": "USD", "input": 0.5, "output": 2.0 });
    let embedding = json!({ "kind": "embedding", "capability_preset": "embedding" });
    gw.priced_model(&fx, "openai/embed", "text-embedding-3-small", embedding.clone(), price.clone()).await;
    gw.priced_model(&fx, "openai/chat", "gpt-mock", json!({}), price.clone()).await;

    let gemini = gw
        .create(
            "/providers",
            json!({ "name": "gemini", "family": "gemini", "managed_api": "native",
                    "base_url": format!("{}/emulate/gemini/v1beta", mock.base_url()) }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{gemini}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
    let vertex = gw
        .create(
            "/providers",
            json!({ "name": "vertex", "family": "gemini", "managed_api": "native", "platform": "vertex",
                    "auth": "google_service_account",
                    "base_url": format!("{}/emulate/vertex/v1/projects/p/locations/europe-west4/publishers/google", mock.base_url()) }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{vertex}/credentials"), json!({ "secret": TEST_SERVICE_ACCOUNT })).await;
    for (provider, name, upstream, extra) in [
        (&gemini, "gemini/embed", "gemini-embedding-001", embedding.clone()),
        (&gemini, "gemini/flash", "gemini-3.8-flash", json!({ "capability_preset": "gemini-3-flash" })),
        (&vertex, "vertex/embed", "text-embedding-005", embedding.clone()),
        (&vertex, "vertex/flash", "gemini-3.8-flash", json!({ "capability_preset": "gemini-3-flash" })),
    ] {
        let id = gw.draft_model(provider, name, upstream, extra).await;
        let (st, v) = gw.api("PUT", &format!("/models/{id}/price"), Some(price.clone())).await;
        assert_eq!(st, 201, "{v}");
        gw.activate(&id).await;
        gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    }
    World { gw, mock, sink, fx }
}

impl World {
    async fn post(&self, path: &str, body: Value) -> (u16, reqwest::header::HeaderMap, Value) {
        let mut req = self.gw.http.post(format!("{}{path}", self.gw.data));
        req = if path.starts_with("/v1/messages") {
            req.header("x-api-key", &self.fx.key).header("anthropic-version", "2023-06-01")
        } else {
            req.bearer_auth(&self.fx.key)
        };
        let r = req.json(&body).send().await.unwrap();
        let (status, headers) = (r.status().as_u16(), r.headers().clone());
        (status, headers, r.json().await.unwrap_or(Value::Null))
    }

    fn sent(&self, suffix: &str) -> Vec<Value> {
        self.mock.captured().into_iter().filter(|c| c.path.ends_with(suffix)).map(|c| c.json().unwrap()).collect()
    }
}

#[tokio::test]
async fn openai_embeddings_are_relayed_with_the_providers_usage() {
    let w = setup().await;
    let (st, headers, v) = w.post("/v1/embeddings", json!({ "model": "openai/embed", "input": ["one", "two"] })).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["data"].as_array().unwrap().len(), 2);
    assert!(headers.get("x-gw-request-id").is_some());
    assert_eq!(w.sent("/v1/embeddings")[0]["model"], "text-embedding-3-small");
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.usage_source.as_str(), e.usage.output_tokens), ("provider", 0));
    assert!(e.usage.input_tokens > 0 && e.cost.unwrap() > 0.0, "{:?}", e.usage);
    assert_eq!(e.path, "/v1/embeddings");
}

#[tokio::test]
async fn gemini_embeds_in_batches_of_a_hundred_and_answers_as_openai() {
    let w = setup().await;
    let texts: Vec<String> = (0..250).map(|i| format!("text number {i}")).collect();
    let (st, headers, v) = w
        .post(
            "/v1/embeddings",
            json!({ "model": "gemini/embed", "input": texts, "dimensions": 8, "encoding_format": "base64" }),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let batches = w.sent(":batchEmbedContents");
    assert_eq!(batches.iter().map(|b| b["requests"].as_array().unwrap().len()).collect::<Vec<_>>(), [100, 100, 50]);
    assert_eq!(batches[0]["requests"][0]["model"], "models/gemini-embedding-001");
    assert_eq!(v["data"].as_array().unwrap().len(), 250);
    assert_eq!(v["data"][249]["index"], 249);
    assert_eq!(v["model"], "gemini/embed");
    let first = base64::engine::general_purpose::STANDARD.decode(v["data"][0]["embedding"].as_str().unwrap()).unwrap();
    assert_eq!(first.len(), 8 * 4, "eight float32s");
    // Gemini reports no usage: the input is estimated (characters / 4), never as output.
    let estimate = texts.iter().map(|t| t.chars().count()).sum::<usize>().div_ceil(4) as u64;
    assert_eq!(v["usage"]["prompt_tokens"], estimate);
    assert!(headers.get("x-gw-cost").is_some());
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.usage_source.as_str(), e.usage.input_tokens, e.usage.output_tokens), ("estimated", estimate, 0));
}

#[tokio::test]
async fn vertex_embeds_with_predict_and_counts_its_tokens() {
    let w = setup().await;
    let (st, _, v) = w.post("/v1/embeddings", json!({ "model": "vertex/embed", "input": ["a b c", "d e"] })).await;
    assert_eq!(st, 200, "{v}");
    let sent = w.sent(":predict");
    assert_eq!(sent[0]["instances"].as_array().unwrap().len(), 2);
    assert!(v["data"][0]["embedding"].is_array());
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!(e.usage_source, "provider", "predict counts each input's tokens");
}

#[tokio::test]
async fn each_endpoint_serves_its_kind_of_model() {
    let w = setup().await;
    let (st, _, v) = w.post("/v1/embeddings", json!({ "model": "openai/chat", "input": "x" })).await;
    assert_eq!(st, 400, "{v}");
    assert!(v.to_string().contains("not an embedding model"), "{v}");
    let (st, _, v) = w
        .post(
            "/v1/chat/completions",
            json!({ "model": "openai/embed", "messages": [{ "role": "user", "content": "x" }] }),
        )
        .await;
    assert_eq!(st, 400, "{v}");
    let (st, _, v) = w.post("/v1/embeddings", json!({ "model": "gemini/embed", "input": [[1, 2, 3]] })).await;
    assert_eq!(st, 400, "token arrays: {v}");
}

#[tokio::test]
async fn anthropic_callers_count_a_gemini_models_tokens() {
    let w = setup().await;
    let body = json!({ "model": "gemini/flash", "system": "Be brief.", "messages": [{ "role": "user", "content": "How many tokens is this?" }] });
    let (st, _, v) = w.post("/v1/messages/count_tokens", body.clone()).await;
    assert_eq!(st, 200, "{v}");
    assert!(v["input_tokens"].as_u64().is_some_and(|n| n > 0), "{v}");
    let sent = &w.sent(":countTokens")[0];
    assert_eq!(sent["generateContentRequest"]["model"], "models/gemini-3.8-flash", "AI Studio's whole form: {sent}");
    // Vertex takes the members flat.
    let mut on_vertex = body;
    on_vertex["model"] = json!("vertex/flash");
    let (st, _, v) = w.post("/v1/messages/count_tokens", on_vertex).await;
    assert_eq!(st, 200, "{v}");
    let sent = w.sent(":countTokens");
    assert!(sent.last().unwrap().get("contents").is_some(), "{:?}", sent.last());
}
