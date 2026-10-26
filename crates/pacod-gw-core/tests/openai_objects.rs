//! OpenAI's Files and Batches through the gateway (concept section 8.3): ownership per
//! workspace, a batch input file read and written for the provider at upload, and the batch's
//! requests accounted one usage event each from its output and error files.

mod common;

use std::time::{Duration, Instant};

use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

struct World {
    gw: TestGw,
    mock: MockProvider,
    sink: IngestSink,
    fx: Fixture,
    other_key: String,
}

/// An OpenAI provider that serves the object APIs, with a priced chat model and an embedding
/// model; Anthropic at the fixture's provider; and a second workspace on the same credential.
async fn setup() -> World {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("anthropic", &mock.base_url()).await;
    let oai = gw
        .create(
            "/providers",
            json!({ "name": "oai", "family": "openai", "base_url": format!("{}/v1", mock.base_url()),
                    "quirks": { "openai_objects": true, "responses_api": true } }),
        )
        .await
        .id;
    gw.create(&format!("/providers/{oai}/credentials"), json!({ "secret": PROVIDER_SECRET })).await;
    let price = json!({ "currency": "USD", "input": 2.0, "output": 10.0, "variants": { "batch": { "input": 1.0, "output": 5.0 } } });
    for (name, upstream, extra) in [
        ("oai/gpt", "gpt-mock", json!({ "capability_preset": "openai-chat" })),
        ("oai/embed", "text-embedding-3-small", json!({ "kind": "embedding", "capability_preset": "embedding" })),
    ] {
        let id = gw.draft_model(&oai, name, upstream, extra).await;
        let (st, v) = gw.api("PUT", &format!("/models/{id}/price"), Some(price.clone())).await;
        assert_eq!(st, 201, "{v}");
        gw.activate(&id).await;
        gw.create(&format!("/orgs/{}/grants", fx.org), json!({ "model_id": id })).await;
    }
    let other = gw.create(&format!("/orgs/{}/workspaces", fx.org), json!({ "name": "other" })).await.id;
    let key = gw.create(&format!("/orgs/{}/workspaces/{other}/keys", fx.org), json!({ "name": "k" })).await;
    let other_key = key.body["secret"].as_str().unwrap().to_owned();
    World { gw, mock, sink, fx, other_key }
}

fn line(custom_id: &str, url: &str, body: Value) -> String {
    json!({ "custom_id": custom_id, "method": "POST", "url": url, "body": body }).to_string()
}

fn chat(model: &str, text: &str) -> Value {
    json!({ "model": model, "messages": [{ "role": "user", "content": text }] })
}

impl World {
    async fn answer(&self, r: reqwest::RequestBuilder) -> (u16, Value) {
        let r = r.send().await.unwrap();
        let status = r.status().as_u16();
        let text = r.text().await.unwrap();
        (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
    }

    fn req(&self, method: reqwest::Method, path: &str, key: &str) -> reqwest::RequestBuilder {
        self.gw.http.request(method, format!("{}{path}", self.gw.data)).bearer_auth(key)
    }

    async fn upload(&self, key: &str, purpose: &str, content: &str) -> (u16, Value) {
        let boundary = "oaiboundary";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"purpose\"\r\n\r\n{purpose}\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"in.jsonl\"\r\nContent-Type: application/jsonl\r\n\r\n{content}\r\n--{boundary}--\r\n"
        );
        self.answer(
            self.req(reqwest::Method::POST, "/v1/files", key)
                .header("content-type", format!("multipart/form-data; boundary={boundary}"))
                .body(body),
        )
        .await
    }

    /// The batch input file as the provider received it.
    fn uploaded(&self) -> String {
        let sent = self.mock.captured().into_iter().filter(|c| c.path == "/v1/files").last().unwrap();
        String::from_utf8_lossy(&sent.body).into_owned()
    }
}

#[tokio::test]
async fn a_batch_file_is_written_for_the_provider_and_kept_to_its_workspace() {
    let w = setup().await;
    let content = [
        line("a", "/v1/chat/completions", chat("oai/gpt", "one")),
        line("b", "/v1/chat/completions", chat("oai/gpt", "two")),
    ]
    .join("\n");
    let (st, file) = w.upload(&w.fx.key, "batch", &content).await;
    assert_eq!(st, 200, "{file}");
    let id = file["id"].as_str().unwrap().to_owned();
    assert_eq!(file["purpose"], "batch");
    // The provider got its own model id, the rest of each request as it was.
    let sent = w.uploaded();
    assert!(sent.contains("\"model\":\"gpt-mock\"") && !sent.contains("oai/gpt"), "{sent}");
    assert!(sent.contains("\"custom_id\":\"a\"") && sent.contains("\"content\":\"two\""), "{sent}");

    // Listed for its workspace in OpenAI's shape, and nowhere else.
    let (_, mine) = w.answer(w.req(reqwest::Method::GET, "/v1/files", &w.fx.key)).await;
    assert_eq!((mine["object"].as_str(), mine["data"][0]["id"].as_str()), (Some("list"), Some(id.as_str())));
    let (_, theirs) = w.answer(w.req(reqwest::Method::GET, "/v1/files", &w.other_key)).await;
    assert_eq!(theirs["data"], json!([]));
    let (st, _) = w.answer(w.req(reqwest::Method::GET, &format!("/v1/files/{id}"), &w.other_key)).await;
    assert_eq!(st, 404);
    // Anthropic's list does not show it either.
    let (_, anthropic) =
        w.answer(w.req(reqwest::Method::GET, "/v1/files", &w.fx.key).header("anthropic-version", "2023-06-01")).await;
    assert_eq!(anthropic["data"], json!([]));
    // A chat call naming another workspace's file is refused as if it did not exist.
    let body = json!({ "model": "oai/gpt", "messages": [{ "role": "user", "content": [
        { "type": "file", "file": { "file_id": id } }, { "type": "text", "text": "read it" }] }] });
    let (st, v) = w.answer(w.req(reqwest::Method::POST, "/v1/chat/completions", &w.other_key).json(&body)).await;
    assert_eq!(st, 404, "{v}");
}

#[tokio::test]
async fn a_request_the_file_cannot_serve_refuses_the_upload() {
    let w = setup().await;
    for (content, word) in [
        (line("a", "/v1/chat/completions", chat("anthropic/claude-test", "x")), "not served by the file's provider"),
        (
            line(
                "a",
                "/v1/chat/completions",
                json!({ "model": "oai/gpt", "stream": true, "messages": [{ "role": "user", "content": "x" }] }),
            ),
            "stream",
        ),
        (
            [
                line("a", "/v1/chat/completions", chat("oai/gpt", "x")),
                line("a", "/v1/chat/completions", chat("oai/gpt", "y")),
            ]
            .join("\n"),
            "used twice",
        ),
        (
            [
                line("a", "/v1/chat/completions", chat("oai/gpt", "x")),
                line("b", "/v1/embeddings", json!({ "model": "oai/embed", "input": "x" })),
            ]
            .join("\n"),
            "one endpoint",
        ),
        (
            line("a", "/v1/responses", json!({ "model": "oai/gpt", "input": "x", "previous_response_id": "resp_1" })),
            "previous_response_id",
        ),
    ] {
        let (st, v) = w.upload(&w.fx.key, "batch", &content).await;
        assert_eq!(st, 400, "{content}: {v}");
        assert!(v["error"]["message"].as_str().unwrap().contains(word), "{word}: {v}");
    }
    assert!(w.mock.captured().iter().all(|c| c.path != "/v1/files"), "nothing was uploaded");

    // A file of another purpose goes as it came, and cannot start a batch.
    let (st, file) = w.upload(&w.fx.key, "user_data", "not a batch").await;
    assert_eq!(st, 200, "{file}");
    let body = json!({ "input_file_id": file["id"], "endpoint": "/v1/chat/completions", "completion_window": "24h" });
    let (st, v) = w.answer(w.req(reqwest::Method::POST, "/v1/batches", &w.fx.key).json(&body)).await;
    assert_eq!(st, 400, "{v}");
    assert!(v["error"]["message"].as_str().unwrap().contains("purpose batch"), "{v}");
}

#[tokio::test]
async fn a_batch_runs_on_its_file_and_each_request_is_accounted_at_the_batch_rate() {
    let w = setup().await;
    let content = [
        line("ok-1", "/v1/chat/completions", chat("oai/gpt", "hello")),
        line("error-1", "/v1/chat/completions", chat("oai/gpt", "fails")),
    ]
    .join("\n");
    let (_, file) = w.upload(&w.fx.key, "batch", &content).await;
    let body = json!({ "input_file_id": file["id"], "endpoint": "/v1/chat/completions", "completion_window": "24h" });
    let (st, batch) = w.answer(w.req(reqwest::Method::POST, "/v1/batches", &w.fx.key).json(&body)).await;
    assert_eq!(st, 200, "{batch}");
    let id = batch["id"].as_str().unwrap().to_owned();
    let (_, listed) = w.answer(w.req(reqwest::Method::GET, "/v1/batches", &w.fx.key)).await;
    assert_eq!(listed["data"][0]["id"], id.as_str());
    let (st, _) = w.answer(w.req(reqwest::Method::GET, &format!("/v1/batches/{id}"), &w.other_key)).await;
    assert_eq!(st, 404);

    // Settled by the gateway's upkeep: one event per request, at the batch rate.
    let of_batch = || w.sink.events().into_iter().filter(|e| e.trace_id == id).collect::<Vec<_>>();
    let deadline = Instant::now() + Duration::from_secs(20);
    while of_batch().len() < 2 {
        assert!(Instant::now() < deadline, "the batch was not accounted: {:?}", of_batch());
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let events = of_batch();
    let ok = events.iter().find(|e| e.status == "ok").unwrap();
    assert_eq!(
        (ok.interface.as_str(), ok.path.as_str(), ok.model_served.as_str()),
        ("openai", "/v1/batches", "oai/gpt")
    );
    assert_eq!((ok.price_variant.as_str(), ok.usage_source.as_str()), ("batch", "provider"));
    let expected = (ok.usage.input_tokens as f64 * 1.0 + ok.usage.output_tokens as f64 * 5.0) / 1e6;
    assert!((ok.cost.unwrap() - expected).abs() < 1e-12, "{:?} vs {expected}", ok.cost);
    let failed = events.iter().find(|e| e.status == "error").unwrap();
    assert_eq!(failed.error_type, "batch_error");

    // The output file is the workspace's: it reads through the gateway.
    let (_, ended) = w.answer(w.req(reqwest::Method::GET, &format!("/v1/batches/{id}"), &w.fx.key)).await;
    let output = ended["output_file_id"].as_str().unwrap().to_owned();
    let r = w.req(reqwest::Method::GET, &format!("/v1/files/{output}/content"), &w.fx.key).send().await.unwrap();
    assert_eq!(r.status(), 200);
    assert!(r.text().await.unwrap().contains("\"custom_id\":\"ok-1\""));
    let (st, _) = w.answer(w.req(reqwest::Method::GET, &format!("/v1/files/{output}/content"), &w.other_key)).await;
    assert_eq!(st, 404);
}

#[tokio::test]
async fn provider_objects_the_gateway_does_not_record_need_an_own_credential() {
    let w = setup().await;
    let body = json!({ "model": "oai/gpt", "input": "search", "tools": [{ "type": "file_search", "vector_store_ids": ["vs_other"] }] });
    let (st, v) = w.answer(w.req(reqwest::Method::POST, "/v1/responses", &w.fx.key).json(&body)).await;
    assert_eq!(st, 403, "{v}");
    assert_eq!(v["error"]["code"], "own_credential_required", "{v}");
}

impl World {
    async fn audit(&self, audit: Value) {
        let path = format!("/orgs/{}/workspaces/{}", self.fx.org, self.fx.ws);
        let settings = json!({ "name": "prod", "tag_keys": ["purpose", "site"], "settings": { "audit": audit } });
        let (st, v) = self.gw.api("PUT", &path, Some(settings)).await;
        assert_eq!(st, 200, "{v}");
        self.gw.wait_applied().await;
    }
}

#[tokio::test]
async fn a_recorded_batch_file_shows_what_the_gateway_wrote_for_the_provider() {
    let w = setup().await;
    w.audit(json!({ "mode": "on", "objects": true })).await;
    let content = line("a", "/v1/chat/completions", chat("oai/gpt", "one"));
    let (st, file) = w.upload(&w.fx.key, "batch", &content).await;
    assert_eq!(st, 200, "{file}");
    let records = w.sink.wait_audit(1).await;
    let r = &records[0];
    assert_eq!((r.client_request.method.as_str(), r.client_request.url.as_str()), ("POST", "/v1/files"));
    // What the caller uploaded, and the file as the provider got it: its own model id.
    assert!(r.client_request.body.contains("oai/gpt"), "{}", r.client_request.body);
    let sent = &r.provider_requests[0];
    assert!(sent.same_as.is_empty() && sent.body.contains("\"model\":\"gpt-mock\"") && !sent.body.contains("oai/gpt"));
    assert!(!r.truncated);
    let text = serde_json::to_string(r).unwrap();
    assert!(!text.contains(PROVIDER_SECRET) && !text.contains(&w.fx.key));
}
