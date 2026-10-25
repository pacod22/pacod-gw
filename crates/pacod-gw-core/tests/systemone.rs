//! TypeSafe's System One API for applications: managed at `POST /v1/systemone` and relayed in
//! proxy mode at `/proxy/typesafe/v1/systemone`, for decision models the workspace is granted.

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

/// The fixture's OpenAI provider with a chat model, and TypeSafe with Jev, both granted.
async fn setup() -> World {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let db = common::fresh_database().await;
    let gw = TestGw::start_on(&db, Some(&sink), |_| {}).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let price = json!({ "currency": "USD", "input": 0.5, "output": 2.0 });
    gw.priced_model(&fx, "openai/chat", "gpt-mock", json!({}), price).await;

    let typesafe = gw
        .create(
            "/providers",
            json!({ "name": "typesafe", "family": "typesafe", "base_url": format!("{}/v1", mock.base_url()) }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{typesafe}/credentials"), json!({ "label": "shared", "secret": PROVIDER_SECRET }))
        .await;
    let jev = gw
        .draft_model(
            &typesafe,
            "typesafe/jev-1.13.0",
            "jev-1.13.0",
            json!({ "kind": "decision", "capability_preset": "decision" }),
        )
        .await;
    let (st, v) = gw
        .api("PUT", &format!("/models/{jev}/price"), Some(json!({ "currency": "USD", "input": 0.042, "output": 0.0 })))
        .await;
    assert_eq!(st, 201, "price: {v}");
    gw.activate(&jev).await;
    gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": jev })).await;
    World { gw, mock, sink, fx }
}

fn question(model: &str, text: &str) -> Value {
    json!({
        "model": model,
        "state": [{ "from": "user", "text": text }],
        "questions": {
            "topic": {
                "type": "choice",
                "instructions": "What is the message about?",
                "criteria": { "billing": { "what": "Charges and refunds." }, "unknown": { "what": "Anything else." } }
            }
        }
    })
}

impl World {
    async fn post(&self, url: &str, key: &str, body: Value) -> (u16, reqwest::header::HeaderMap, Value) {
        let r = self.gw.http.post(format!("{}{url}", self.gw.data)).bearer_auth(key).json(&body).send().await.unwrap();
        let (status, headers) = (r.status().as_u16(), r.headers().clone());
        (status, headers, r.json().await.unwrap_or(Value::Null))
    }

    fn decided(&self) -> Vec<pacod_gw_testkit::mock::CapturedRequest> {
        self.mock.captured().into_iter().filter(|c| c.path.ends_with("/systemone")).collect()
    }
}

#[tokio::test]
async fn an_application_asks_a_decision_model_in_managed_mode() {
    let w = setup().await;
    let (st, headers, v) =
        w.post("/v1/systemone", &w.fx.key, question("typesafe/jev-1.13.0", "I was charged twice, billing")).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["answers"]["topic"]["choice"], "billing");
    assert!(headers.get("x-gw-request-id").is_some());

    // The provider received its own model id and credential, nothing of the gateway's.
    let sent = w.decided();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].json().unwrap()["model"], "jev-1.13.0");
    assert_eq!(sent[0].header("authorization"), Some(format!("Bearer {PROVIDER_SECRET}").as_str()));

    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.interface.as_str(), e.path.as_str(), e.mode.as_str()), ("decision", "/v1/systemone", "managed"));
    assert_eq!((e.model_served.as_str(), e.model_reported.as_str()), ("typesafe/jev-1.13.0", "jev-1.13.0"));
    assert_eq!(e.usage_source, "provider");
    assert!(e.usage.input_tokens > 0, "{:?}", e.usage);
    assert!(e.cost.is_some_and(|c| c > 0.0) && !e.price_missing, "{:?}", e.cost);
    assert_eq!(e.key_id, w.fx.key_id);
    assert!(e.parent_id.is_empty(), "an application's own call, not a plugin's");
}

#[tokio::test]
async fn decision_models_and_chat_models_keep_to_their_endpoints() {
    let w = setup().await;
    // A chat model cannot answer System One's questions...
    let (st, _, v) = w.post("/v1/systemone", &w.fx.key, question("openai/chat", "hello")).await;
    assert_eq!(st, 400, "{v}");
    assert_eq!(v["error"]["message"], "the model \"openai/chat\" is not a decision model");
    // ... and a decision model does not chat.
    let (st, _, v) = w
        .post(
            "/v1/chat/completions",
            &w.fx.key,
            json!({ "model": "typesafe/jev-1.13.0", "messages": [{ "role": "user", "content": "hi" }] }),
        )
        .await;
    assert_eq!(st, 400, "{v}");
    assert_eq!(v["error"]["message"], "the model \"typesafe/jev-1.13.0\" is not a chat model");
    // Model lists, which SDKs offer for chat, leave decision models out.
    let r = w.gw.http.get(format!("{}/v1/models", w.gw.data)).bearer_auth(&w.fx.key).send().await.unwrap();
    assert!(!r.text().await.unwrap().contains("jev"));
    assert!(w.decided().is_empty());
}

#[tokio::test]
async fn a_key_that_does_not_list_the_model_is_refused() {
    let w = setup().await;
    let narrow = w.gw.key(&w.fx, json!({ "name": "narrow", "allowed_models": ["openai/chat"] })).await;
    let (st, _, v) = w.post("/v1/systemone", &narrow, question("typesafe/jev-1.13.0", "hello")).await;
    assert_eq!(st, 403, "{v}");
    let (st, _, v) = w.post("/proxy/typesafe/v1/systemone", &narrow, question("jev-1.13.0", "hello")).await;
    assert_eq!(st, 403, "{v}");
    assert!(w.decided().is_empty());
}

#[tokio::test]
async fn proxy_mode_relays_system_one_and_reads_its_usage() {
    let w = setup().await;
    let (st, _, v) =
        w.post("/proxy/typesafe/v1/systemone", &w.fx.key, question("jev-1.13.0", "a billing question")).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["answers"]["topic"]["choice"], "billing");
    let sent = w.decided();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].header("authorization"), Some(format!("Bearer {PROVIDER_SECRET}").as_str()));

    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.mode.as_str(), e.model_served.as_str()), ("proxy", "typesafe/jev-1.13.0"));
    assert_eq!(e.usage_source, "provider");
    assert!(e.usage.input_tokens > 0 && e.usage.output_tokens > 0, "{:?}", e.usage);
    assert!(e.cost.is_some_and(|c| c > 0.0), "{:?}", e.cost);
}
