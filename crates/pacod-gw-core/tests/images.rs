//! Image generation and edits in managed mode (`/v1/images/generations`, `/v1/images/edits`):
//! relayed to OpenAI-shaped providers, translated to Imagen and to Gemini's image models.

mod common;

use bytes::Bytes;
use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_dialects::multipart::{OwnedPart, content_type, write};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

struct World {
    gw: TestGw,
    mock: MockProvider,
    sink: IngestSink,
    fx: Fixture,
}

/// gpt-image and DALL·E at the fixture's OpenAI provider; Imagen and a Gemini image model at
/// Gemini's own API; a chat model to be refused.
async fn setup() -> World {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    let image = json!({ "kind": "image", "capability_preset": "image" });
    gw.priced_model(
        &fx,
        "openai/gpt-image",
        "gpt-image-1",
        image.clone(),
        json!({ "currency": "USD", "input": 5.0, "input_image": 10.0, "output": 40.0 }),
    )
    .await;
    let dalle = gw.draft_model(&fx.provider, "openai/dalle", "dall-e-3", image.clone()).await;
    let price = json!({ "currency": "USD", "per_image": 0.04, "variants": { "hd-1024x1792": { "per_image": 0.12 } } });
    let (st, v) = gw.api("PUT", &format!("/models/{dalle}/price"), Some(price)).await;
    assert_eq!(st, 201, "{v}");
    gw.activate(&dalle).await;
    gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": dalle })).await;
    gw.priced_model(
        &fx,
        "openai/chat",
        "gpt-mock",
        json!({}),
        json!({ "currency": "USD", "input": 1.0, "output": 2.0 }),
    )
    .await;

    let gemini = gw
        .create(
            "/providers",
            json!({ "name": "gemini", "family": "gemini", "managed_api": "native",
                    "base_url": format!("{}/emulate/gemini/v1beta", mock.base_url()) }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{gemini}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
    for (name, upstream, price) in [
        ("gemini/imagen", "imagen-4.0-generate-001", json!({ "currency": "USD", "per_image": 0.03 })),
        (
            "gemini/image",
            "gemini-3.8-flash-image",
            json!({ "currency": "USD", "input": 0.3, "output": 2.5, "output_image": 30.0 }),
        ),
    ] {
        let id = gw.draft_model(&gemini, name, upstream, image.clone()).await;
        let (st, v) = gw.api("PUT", &format!("/models/{id}/price"), Some(price)).await;
        assert_eq!(st, 201, "{v}");
        gw.activate(&id).await;
        gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    }
    World { gw, mock, sink, fx }
}

impl World {
    async fn generate(&self, body: Value) -> (u16, Value) {
        let r = self
            .gw
            .http
            .post(format!("{}/v1/images/generations", self.gw.data))
            .bearer_auth(&self.fx.key)
            .json(&body)
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    async fn edit(&self, model: &str, extra: &[(&str, &str)]) -> (u16, Value) {
        let mut parts = vec![
            OwnedPart {
                name: "model".into(),
                filename: None,
                content_type: None,
                body: Bytes::copy_from_slice(model.as_bytes()),
            },
            OwnedPart {
                name: "prompt".into(),
                filename: None,
                content_type: None,
                body: Bytes::from_static(b"add a hat"),
            },
            OwnedPart {
                name: "image".into(),
                filename: Some("cat.png".into()),
                content_type: Some("image/png".into()),
                body: Bytes::from_static(b"\x89PNG\r\n\x1a\nCAT"),
            },
        ];
        for (k, v) in extra {
            parts.push(OwnedPart {
                name: (*k).into(),
                filename: None,
                content_type: None,
                body: Bytes::copy_from_slice(v.as_bytes()),
            });
        }
        let r = self
            .gw
            .http
            .post(format!("{}/v1/images/edits", self.gw.data))
            .bearer_auth(&self.fx.key)
            .header("content-type", content_type("test-boundary"))
            .body(write("test-boundary", &parts))
            .send()
            .await
            .unwrap();
        (r.status().as_u16(), r.json().await.unwrap_or(Value::Null))
    }

    fn sent(&self, suffix: &str) -> Vec<pacod_gw_testkit::mock::CapturedRequest> {
        self.mock.captured().into_iter().filter(|c| c.path.ends_with(suffix)).collect()
    }
}

#[tokio::test]
async fn gpt_image_is_relayed_and_priced_by_its_tokens() {
    let w = setup().await;
    let (st, v) = w.generate(json!({ "model": "openai/gpt-image", "prompt": "a cat", "n": 2 })).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["data"].as_array().unwrap().len(), 2);
    assert!(v["data"][0]["b64_json"].is_string());
    assert_eq!(w.sent("/v1/images/generations")[0].json().unwrap()["model"], "gpt-image-1");
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.path.as_str(), e.usage_source.as_str()), ("/v1/images/generations", "provider"));
    assert_eq!((e.usage.output_tokens, e.usage.image_output_tokens), (2112, 2112));
    // Two images at 1056 tokens each, at $40 a million, and the prompt's text tokens at $5.
    let expected = (e.usage.input_tokens as f64 * 5.0 + 2112.0 * 40.0) / 1e6;
    assert!((e.cost.unwrap() - expected).abs() < 1e-9, "{:?} vs {expected}", e.cost);
}

#[tokio::test]
async fn dall_e_is_priced_per_image_and_by_its_size_and_quality() {
    let w = setup().await;
    let (st, v) = w.generate(json!({ "model": "openai/dalle", "prompt": "a cat" })).await;
    assert_eq!(st, 200, "{v}");
    assert!(v["data"][0]["url"].is_string(), "DALL·E answers a URL by default: {v}");
    let (st, _) = w
        .generate(json!({ "model": "openai/dalle", "prompt": "a cat", "quality": "hd", "size": "1024x1792", "n": 2 }))
        .await;
    assert_eq!(st, 200);
    let events = w.sink.wait_events(2).await;
    let standard = events.iter().find(|e| e.usage.images == 1).unwrap();
    assert_eq!((standard.usage_source.as_str(), standard.cost), ("estimated", Some(0.04)));
    let hd = events.iter().find(|e| e.usage.images == 2).unwrap();
    assert_eq!((hd.price_variant.as_str(), hd.cost), ("hd-1024x1792", Some(0.24)));
}

#[tokio::test]
async fn an_edit_reaches_the_provider_as_the_caller_sent_it_but_for_its_model() {
    let w = setup().await;
    let (st, v) = w.edit("openai/gpt-image", &[("size", "1024x1024")]).await;
    assert_eq!(st, 200, "{v}");
    assert!(v["data"][0]["b64_json"].is_string());
    let sent = w.sent("/v1/images/edits").remove(0);
    assert!(sent.header("content-type").unwrap().contains("boundary=test-boundary"));
    let body = String::from_utf8_lossy(&sent.body).to_string();
    assert!(body.contains("\r\n\r\ngpt-image-1\r\n"), "{body}");
    assert!(!body.contains("openai/gpt-image"));
    assert!(sent.body.windows(11).any(|w| w == b"\x89PNG\r\n\x1a\nCAT"), "the image keeps its bytes");
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!(e.path, "/v1/images/edits");
    assert!(e.usage.image_input_tokens > 0, "{:?}", e.usage);
}

#[tokio::test]
async fn imagen_is_reached_through_predict_and_answers_base64() {
    let w = setup().await;
    let (st, v) = w.generate(json!({ "model": "gemini/imagen", "prompt": "a cat", "n": 3, "size": "1536x1024" })).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["data"].as_array().unwrap().len(), 3);
    assert!(v["data"][0]["b64_json"].as_str().unwrap().starts_with("iVBORw0KGgo"), "a PNG: {v}");
    let sent = w.sent(":predict");
    assert_eq!(sent.len(), 1);
    let body = sent[0].json().unwrap();
    assert_eq!(
        (body["parameters"]["sampleCount"].as_u64(), body["parameters"]["aspectRatio"].as_str()),
        (Some(3), Some("16:9"))
    );
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.usage.images, e.cost), (3, Some(0.09)));

    // What Imagen cannot give is refused, and nothing is sent.
    let (st, v) = w.generate(json!({ "model": "gemini/imagen", "prompt": "a cat", "response_format": "url" })).await;
    assert_eq!((st, v["error"]["code"].as_str()), (400, Some("unsupported_feature")), "{v}");
    assert_eq!(w.sent(":predict").len(), 1);
}

#[tokio::test]
async fn a_gemini_image_model_makes_and_edits_images_priced_by_its_tokens() {
    let w = setup().await;
    let (st, v) = w.generate(json!({ "model": "gemini/image", "prompt": "a cat", "n": 2 })).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["data"].as_array().unwrap().len(), 2);
    assert_eq!(w.sent(":generateContent").len(), 2, "one call per image");
    assert_eq!(v["usage"]["output_tokens"], 2590);
    assert!(v["usage"]["cost"]["amount"].is_number(), "{v}");
    let e = w.sink.wait_events(1).await.remove(0);
    assert_eq!((e.usage.image_output_tokens, e.usage_source.as_str()), (2580, "provider"));

    let (st, v) = w.edit("gemini/image", &[]).await;
    assert_eq!(st, 200, "{v}");
    let sent = w.sent(":generateContent").pop().unwrap().json().unwrap();
    assert_eq!(sent["contents"][0]["parts"][1]["inlineData"]["mimeType"], "image/png");
}

#[tokio::test]
async fn image_models_and_chat_models_keep_to_their_endpoints() {
    let w = setup().await;
    let (st, v) = w.generate(json!({ "model": "openai/chat", "prompt": "a cat" })).await;
    assert_eq!((st, v["error"]["message"].as_str()), (400, Some("the model \"openai/chat\" is not an image model")));
    let r =
        w.gw.http
            .post(format!("{}/v1/chat/completions", w.gw.data))
            .bearer_auth(&w.fx.key)
            .json(&json!({ "model": "openai/gpt-image", "messages": [{ "role": "user", "content": "hi" }] }))
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 400);
    let (st, _) = w.edit("openai/gpt-image", &[]).await;
    assert_eq!(st, 200);
    // A body that is not a form is refused before anything is sent.
    let r =
        w.gw.http
            .post(format!("{}/v1/images/edits", w.gw.data))
            .bearer_auth(&w.fx.key)
            .json(&json!({ "model": "openai/gpt-image" }))
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 400);
}
