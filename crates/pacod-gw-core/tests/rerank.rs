//! Rerank in managed mode (`POST /v1/rerank`, `/v2/rerank`): Cohere's shape for callers, each
//! provider asked in its own (Cohere, Jina, Voyage), priced per search unit or by tokens.

mod common;

use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

struct World {
    gw: TestGw,
    mock: MockProvider,
    sink: IngestSink,
    fx: Fixture,
}

/// Cohere priced per search unit, Jina and Voyage by tokens, and a chat model.
async fn setup() -> World {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let kind = json!({ "kind": "rerank", "capability_preset": "rerank" });
    gw.priced_model(
        &fx,
        "openai/chat",
        "gpt-mock",
        json!({}),
        json!({ "currency": "USD", "input": 1.0, "output": 1.0 }),
    )
    .await;
    for (name, family, base, quirks, model, upstream, price) in [
        (
            "cohere",
            "cohere",
            mock.base_url(),
            json!({}),
            "cohere/rerank",
            "rerank-v3.5",
            json!({ "currency": "USD", "per_search_unit": 0.002 }),
        ),
        (
            "jina",
            "openai",
            format!("{}/v1", mock.base_url()),
            json!({}),
            "jina/rerank",
            "jina-reranker-v2",
            json!({ "currency": "USD", "input": 0.02 }),
        ),
        (
            "voyage",
            "openai",
            format!("{}/voyage/v1", mock.base_url()),
            json!({ "rerank_api": "voyage" }),
            "voyage/rerank",
            "rerank-2.5",
            json!({ "currency": "USD", "input": 0.05 }),
        ),
    ] {
        let provider = gw
            .create("/providers", json!({ "name": name, "family": family, "base_url": base, "quirks": quirks }))
            .await
            .id;
        gw.create(&format!("/providers/{provider}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
        let id = gw.draft_model(&provider, model, upstream, kind.clone()).await;
        let (st, v) = gw.api("PUT", &format!("/models/{id}/price"), Some(price)).await;
        assert_eq!(st, 201, "{v}");
        gw.activate(&id).await;
        gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    }
    World { gw, mock, sink, fx }
}

fn ask(model: &str) -> Value {
    json!({
        "model": model,
        "query": "capital of France",
        "documents": ["Berlin is a city", { "text": "Paris is the capital of France" }, "France has wine"],
        "top_n": 2,
        "return_documents": true
    })
}

impl World {
    async fn rerank(&self, path: &str, body: Value) -> (u16, Value) {
        let r = self
            .gw
            .http
            .post(format!("{}{path}", self.gw.data))
            .bearer_auth(&self.fx.key)
            .json(&body)
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    fn sent(&self, suffix: &str) -> Vec<Value> {
        self.mock.captured().into_iter().filter(|c| c.path.ends_with(suffix)).map(|c| c.json().unwrap()).collect()
    }
}

#[tokio::test]
async fn cohere_is_asked_in_its_v2_shape_and_priced_per_search_unit() {
    let w = setup().await;
    let (st, v) = w.rerank("/v1/rerank", ask("cohere/rerank")).await;
    assert_eq!(st, 200, "{v}");
    let results = v["results"].as_array().unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(
        (results[0]["index"].as_u64(), results[0]["document"]["text"].as_str()),
        (Some(1), Some("Paris is the capital of France"))
    );
    assert_eq!(v["meta"]["billed_units"]["search_units"], 1);
    let sent = w.sent("/v2/rerank");
    assert_eq!(sent[0]["model"], "rerank-v3.5");
    assert_eq!(sent[0]["documents"][1], "Paris is the capital of France", "Cohere's v2 takes strings");
    assert!(sent[0].get("return_documents").is_none());
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.path.as_str(), e.usage.search_units, e.cost), ("/v1/rerank", 1, Some(0.002)));
    assert_eq!(e.usage_source, "provider");

    // Cohere's own SDK calls /v2/rerank: the same endpoint.
    let (st, _) = w.rerank("/v2/rerank", ask("cohere/rerank")).await;
    assert_eq!(st, 200);
}

#[tokio::test]
async fn jina_and_voyage_are_priced_by_their_tokens() {
    let w = setup().await;
    let (st, v) = w.rerank("/v1/rerank", ask("jina/rerank")).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["results"][0]["index"], 1);
    assert!(v["usage"]["total_tokens"].as_u64().is_some_and(|n| n > 0), "{v}");
    let (st, v) = w.rerank("/v1/rerank", ask("voyage/rerank")).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["results"].as_array().unwrap().len(), 2, "Voyage's data read back as results");
    let sent = w.sent("/voyage/v1/rerank");
    assert_eq!((sent[0]["top_k"].as_u64(), sent[0].get("top_n")), (Some(2), None));
    let events = w.sink.wait_events(2).await;
    assert!(events.iter().all(|e| e.usage.input_tokens > 0 && e.cost.is_some_and(|c| c > 0.0)), "{events:?}");
}

#[tokio::test]
async fn only_rerank_models_rank_and_proxy_mode_reads_their_usage() {
    let w = setup().await;
    let (st, v) = w.rerank("/v1/rerank", ask("openai/chat")).await;
    assert_eq!((st, v["error"]["message"].as_str()), (400, Some("the model \"openai/chat\" is not a rerank model")));
    let (st, v) = w.rerank("/v1/rerank", json!({ "model": "cohere/rerank", "query": "x", "documents": [] })).await;
    assert_eq!(st, 400, "{v}");

    let (st, v) = w
        .rerank(
            "/proxy/cohere/v2/rerank",
            json!({ "model": "rerank-v3.5", "query": "France", "documents": ["France"] }),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let e = w.sink.wait_events(3).await.into_iter().find(|e| e.mode == "proxy").unwrap();
    assert_eq!((e.usage.search_units, e.cost), (1, Some(0.002)));
}
