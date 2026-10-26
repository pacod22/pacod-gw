//! Anthropic's Files and Message Batches through the gateway (concept section 8.3): ownership
//! per workspace, batch items governed like single messages, and batch results accounted as
//! one usage event per item.

mod common;

use std::time::{Duration, Instant};

use common::{Fixture, IngestSink, TestGw};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};

async fn setup() -> (TestGw, MockProvider, IngestSink, Fixture, String) {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("anthropic", &mock.base_url()).await;
    // A second workspace of the same organisation, on the same shared credential.
    let other = gw.create(&format!("/orgs/{}/workspaces", fx.org), json!({ "name": "other" })).await.id;
    let key = gw.create(&format!("/orgs/{}/workspaces/{other}/keys", fx.org), json!({ "name": "k" })).await;
    let other_key = key.body["secret"].as_str().unwrap().to_owned();
    (gw, mock, sink, fx, other_key)
}

/// A request the way the Anthropic SDK sends it: the key in `x-api-key`, and the version.
fn anthropic(gw: &TestGw, method: reqwest::Method, path: &str, key: &str) -> reqwest::RequestBuilder {
    gw.http
        .request(method, format!("{}{path}", gw.data))
        .header("x-api-key", key)
        .header("anthropic-version", "2023-06-01")
}

async fn answer(r: reqwest::RequestBuilder) -> (u16, Value) {
    let r = r.send().await.unwrap();
    let status = r.status().as_u16();
    let text = r.text().await.unwrap();
    (status, serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

async fn upload(gw: &TestGw, key: &str, name: &str, content: &str) -> (u16, Value) {
    let boundary = "pgwtestboundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: text/plain\r\n\r\n{content}\r\n--{boundary}--\r\n"
    );
    answer(
        anthropic(gw, reqwest::Method::POST, "/v1/files", key)
            .header("content-type", format!("multipart/form-data; boundary={boundary}"))
            .body(body),
    )
    .await
}

fn message_with(file_id: &str) -> Value {
    json!({ "model": "anthropic/claude-test", "max_tokens": 50, "messages": [{ "role": "user", "content": [
        { "type": "document", "source": { "type": "file", "file_id": file_id } },
        { "type": "text", "text": "summarise" }
    ] }] })
}

#[tokio::test]
async fn files_belong_to_the_workspace_that_uploaded_them() {
    let (gw, mock, _sink, fx, other_key) = setup().await;
    let (st, file) = upload(&gw, &fx.key, "notes.txt", "hello file").await;
    assert_eq!(st, 200, "{file}");
    let id = file["id"].as_str().unwrap().to_owned();
    assert_eq!((file["filename"].as_str(), file["size_bytes"].as_u64()), (Some("notes.txt"), Some(10)));
    // The provider got the upload as it was sent, with the gateway's credential.
    let sent = mock.captured().into_iter().find(|c| c.path == "/v1/files").unwrap();
    assert_eq!(sent.header("x-api-key"), Some(common::PROVIDER_SECRET));
    assert!(sent.header("content-type").unwrap().starts_with("multipart/form-data; boundary="));

    // Listed for its workspace, and nowhere else.
    let (_, mine) = answer(anthropic(&gw, reqwest::Method::GET, "/v1/files", &fx.key)).await;
    assert_eq!(mine["data"][0]["id"], id.as_str());
    assert_eq!((mine["has_more"].as_bool(), mine["first_id"].as_str()), (Some(false), Some(id.as_str())));
    let (_, theirs) = answer(anthropic(&gw, reqwest::Method::GET, "/v1/files", &other_key)).await;
    assert_eq!(theirs["data"], json!([]));

    // Another workspace cannot read, download, delete or use it: it does not exist for them.
    for (method, path) in [
        (reqwest::Method::GET, format!("/v1/files/{id}")),
        (reqwest::Method::GET, format!("/v1/files/{id}/content")),
        (reqwest::Method::DELETE, format!("/v1/files/{id}")),
    ] {
        let (st, v) = answer(anthropic(&gw, method, &path, &other_key)).await;
        assert_eq!(st, 404, "{path}: {v}");
    }
    let (st, v) =
        answer(anthropic(&gw, reqwest::Method::POST, "/v1/messages", &other_key).json(&message_with(&id))).await;
    assert_eq!((st, v["error"]["type"].as_str()), (404, Some("not_found_error")), "{v}");
    // Not with the key escaped, which the provider reads as file_id, nor to count its tokens.
    let escaped = message_with(&id).to_string().replace("\"file_id\"", "\"file\\u005fid\"");
    assert!(escaped.contains("file\\u005fid"));
    let (st, v) = answer(
        anthropic(&gw, reqwest::Method::POST, "/v1/messages", &other_key)
            .header("content-type", "application/json")
            .body(escaped),
    )
    .await;
    assert_eq!(st, 404, "{v}");
    let mut count = message_with(&id);
    count.as_object_mut().unwrap().remove("max_tokens");
    let (st, v) =
        answer(anthropic(&gw, reqwest::Method::POST, "/v1/messages/count_tokens", &other_key).json(&count)).await;
    assert_eq!(st, 404, "{v}");
    // Nor can a file the gateway does not know be named on the shared credential.
    let (st, _) =
        answer(anthropic(&gw, reqwest::Method::POST, "/v1/messages", &fx.key).json(&message_with("file_unknown")))
            .await;
    assert_eq!(st, 404);
    // The owner can, from the OpenAI interface too.
    let (st, v) = answer(anthropic(&gw, reqwest::Method::POST, "/v1/messages", &fx.key).json(&message_with(&id))).await;
    assert_eq!(st, 200, "{v}");

    let content =
        anthropic(&gw, reqwest::Method::GET, &format!("/v1/files/{id}/content"), &fx.key).send().await.unwrap();
    assert_eq!(content.status(), 200);
    assert_eq!(content.text().await.unwrap(), "hello file");
    let (st, deleted) = answer(anthropic(&gw, reqwest::Method::DELETE, &format!("/v1/files/{id}"), &fx.key)).await;
    assert_eq!((st, deleted["type"].as_str()), (200, Some("file_deleted")));
    let (st, _) = answer(anthropic(&gw, reqwest::Method::GET, &format!("/v1/files/{id}"), &fx.key)).await;
    assert_eq!(st, 404, "gone once deleted");

    // Without Anthropic's version header it is OpenAI's Files API, whose list holds OpenAI's
    // files only.
    let r = gw.http.get(format!("{}/v1/files", gw.data)).bearer_auth(&fx.key).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let openai: Value = r.json().await.unwrap();
    assert_eq!((openai["object"].as_str(), &openai["data"]), (Some("list"), &json!([])));
    // The management API lists them per workspace.
    let (_, listed) = gw.api("GET", &format!("/orgs/{}/workspaces/{}/files", fx.org, fx.ws), None).await;
    assert_eq!((listed["items"][0]["id"].as_str(), listed["items"].as_array().unwrap().len()), (Some(id.as_str()), 1));
}

fn batch(items: &[(&str, &str)]) -> Value {
    json!({ "requests": items.iter().map(|(id, model)| json!({ "custom_id": id, "params": {
        "model": model, "max_tokens": 50, "messages": [{ "role": "user", "content": format!("hello {id}") }]
    } })).collect::<Vec<_>>() })
}

#[tokio::test]
async fn batch_items_are_governed_and_accounted_one_event_each() {
    let (gw, mock, sink, fx, other_key) = setup().await;
    let rates = json!({ "currency": "USD", "input": 4.0, "output": 20.0, "variants": { "batch": { "input": 2.0, "output": 10.0 } } });
    gw.priced_model(&fx, "anthropic/priced", "priced", json!({}), rates).await;
    let path = "/v1/messages/batches";

    // What a batch may not hold.
    for (bad, why) in [
        (batch(&[("a", "anthropic/priced"), ("a", "anthropic/priced")]), "used twice"),
        (batch(&[("not ok", "anthropic/priced")]), "custom_id"),
        (json!({ "requests": [] }), "between 1 and"),
    ] {
        let (st, v) = answer(anthropic(&gw, reqwest::Method::POST, path, &fx.key).json(&bad)).await;
        assert_eq!(st, 400, "{why}: {v}");
        assert!(v.to_string().contains(why), "{why}: {v}");
    }
    let mut streaming = batch(&[("s", "anthropic/priced")]);
    streaming["requests"][0]["params"]["stream"] = json!(true);
    assert_eq!(answer(anthropic(&gw, reqwest::Method::POST, path, &fx.key).json(&streaming)).await.0, 400);

    // Limits are checked when the batch is created: each item is a request.
    let limits = format!("/orgs/{}/workspaces/{}/limits", fx.org, fx.ws);
    let limit = gw
        .create(
            &limits,
            json!({ "name": "day", "subject": "workspace", "metric": "requests", "window": "day", "amount": 3.0 }),
        )
        .await
        .id;
    let too_many = batch(&[
        ("a", "anthropic/priced"),
        ("b", "anthropic/priced"),
        ("c", "anthropic/priced"),
        ("d", "anthropic/priced"),
    ]);
    let (st, v) = answer(anthropic(&gw, reqwest::Method::POST, path, &fx.key).json(&too_many)).await;
    assert_eq!(st, 402, "{v}");

    // A batch of three: one answer, one error, one expired. Nothing was counted by the refusal.
    let (st, created) = answer(anthropic(&gw, reqwest::Method::POST, path, &fx.key).json(&batch(&[
        ("ok-1", "anthropic/priced"),
        ("error-2", "anthropic/priced"),
        ("expire-3", "anthropic/priced"),
    ])))
    .await;
    assert_eq!(st, 200, "{created}");
    let id = created["id"].as_str().unwrap().to_owned();
    assert_eq!(created["processing_status"], "in_progress");
    // The provider got the items as single messages are sent: model ids resolved.
    let sent: Value =
        serde_json::from_slice(&mock.captured().into_iter().find(|c| c.path == path).unwrap().body).unwrap();
    assert_eq!(sent["requests"][0]["params"]["model"], "priced");
    // The limit is used up by the three items.
    let (_, state) = gw.api("GET", &format!("/orgs/{}/workspaces/{}/limit-state", fx.org, fx.ws), None).await;
    let used = state.as_array().unwrap().iter().find(|s| s["limit_id"] == limit.as_str()).unwrap()["used"].as_f64();
    assert_eq!(used, Some(3.0), "{state}");
    gw.api("DELETE", &format!("{limits}/{limit}"), None).await;

    // Another workspace does not see it.
    let (st, _) = answer(anthropic(&gw, reqwest::Method::GET, &format!("{path}/{id}"), &other_key)).await;
    assert_eq!(st, 404);
    let (_, theirs) = answer(anthropic(&gw, reqwest::Method::GET, path, &other_key)).await;
    assert_eq!(theirs["data"], json!([]));

    // The gateway asks about it until it has ended, then accounts it: one event per item.
    let of_batch = |id: &str| sink.events().into_iter().filter(|e| e.trace_id == id).collect::<Vec<_>>();
    let deadline = Instant::now() + Duration::from_secs(15);
    while of_batch(&id).len() < 3 {
        assert!(Instant::now() < deadline, "the batch was not accounted");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let batch_events = of_batch(&id);
    assert_eq!(batch_events.len(), 3, "{batch_events:?}");
    let ok = batch_events.iter().find(|e| e.status == "ok").unwrap();
    assert_eq!((ok.price_variant.as_str(), ok.usage_source.as_str()), ("batch", "provider"));
    // Dated when the batch ended, but never before it was created: the mock's clock says January.
    assert!(ok.timestamp > chrono::Utc::now() - chrono::Duration::minutes(5), "{}", ok.timestamp);
    let expected = (ok.usage.input_tokens as f64 * 2.0 + ok.usage.output_tokens as f64 * 10.0) / 1e6;
    assert!((ok.cost.unwrap() - expected).abs() < 1e-12, "at the batch rate: {:?}", ok.cost);
    let kinds: Vec<&str> = batch_events.iter().map(|e| e.error_type.as_str()).filter(|t| !t.is_empty()).collect();
    assert!(kinds.contains(&"batch_errored") && kinds.contains(&"batch_expired"), "{kinds:?}");
    // Settled once, with its cost.
    let deadline = Instant::now() + Duration::from_secs(10);
    let settled = loop {
        let (_, listed) = gw.api("GET", &format!("/orgs/{}/workspaces/{}/batches", fx.org, fx.ws), None).await;
        let row = listed["items"][0].clone();
        if row["status"] == "settled" || Instant::now() > deadline {
            break row;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    };
    assert_eq!(settled["status"], "settled", "{settled}");
    assert!((settled["cost"].as_f64().unwrap() - expected).abs() < 1e-12);

    // The results come through the gateway: results_url names the gateway's own path.
    let (_, ended) = answer(anthropic(&gw, reqwest::Method::GET, &format!("{path}/{id}"), &fx.key)).await;
    assert_eq!(ended["results_url"], format!("/v1/messages/batches/{id}/results"));
    let results = anthropic(&gw, reqwest::Method::GET, &format!("{path}/{id}/results"), &fx.key)
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(results.lines().count(), 3, "{results}");
    let (st, deleted) = answer(anthropic(&gw, reqwest::Method::DELETE, &format!("{path}/{id}"), &fx.key)).await;
    assert_eq!((st, deleted["type"].as_str()), (200, Some("message_batch_deleted")), "{deleted}");

    // A cancelled batch: its items are accounted as cancelled, at no cost.
    let (_, second) =
        answer(anthropic(&gw, reqwest::Method::POST, path, &fx.key).json(&batch(&[("later", "anthropic/priced")])))
            .await;
    let second_id = second["id"].as_str().unwrap().to_owned();
    let (st, canceling) =
        answer(anthropic(&gw, reqwest::Method::POST, &format!("{path}/{second_id}/cancel"), &fx.key)).await;
    assert_eq!((st, canceling["processing_status"].as_str()), (200, Some("canceling")));
    let deadline = Instant::now() + Duration::from_secs(15);
    while of_batch(&second_id).is_empty() {
        assert!(Instant::now() < deadline, "the cancelled batch was not accounted");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let cancelled = of_batch(&second_id).remove(0);
    assert_eq!((cancelled.status.as_str(), cancelled.cost), ("cancelled", None));
}

async fn workspace_audit(gw: &TestGw, fx: &Fixture, audit: Value) {
    let path = format!("/orgs/{}/workspaces/{}", fx.org, fx.ws);
    let settings = json!({ "name": "prod", "tag_keys": ["purpose", "site"], "settings": { "audit": audit } });
    let (st, v) = gw.api("PUT", &path, Some(settings)).await;
    assert_eq!(st, 200, "{v}");
    gw.wait_applied().await;
}

/// The record of the call `method url`.
fn record_of<'a>(
    records: &'a [pacod_gw_events::AuditRecord],
    method: &str,
    url: &str,
) -> &'a pacod_gw_events::AuditRecord {
    records
        .iter()
        .find(|r| r.client_request.method == method && r.client_request.url == url)
        .unwrap_or_else(|| panic!("no record of {method} {url}"))
}

#[tokio::test]
async fn audit_mode_records_file_and_batch_calls_where_it_records_them() {
    let (gw, _mock, sink, fx, _) = setup().await;
    gw.priced_model(
        &fx,
        "anthropic/priced",
        "priced",
        json!({}),
        json!({ "currency": "USD", "input": 1.0, "output": 1.0 }),
    )
    .await;
    // Audit mode alone leaves these calls alone.
    workspace_audit(&gw, &fx, json!({ "mode": "on" })).await;
    let (st, file) = upload(&gw, &fx.key, "notes.txt", "hello file").await;
    assert_eq!(st, 200, "{file}");
    let unrecorded = file["id"].as_str().unwrap().to_owned();
    let content = format!("/v1/files/{unrecorded}/content");
    let r = anthropic(&gw, reqwest::Method::GET, &content, &fx.key).send().await.unwrap();
    assert_eq!(r.text().await.unwrap(), "hello file");
    let plain = sink.wait_events(2).await;
    assert!(plain.iter().all(|e| e.audit_id.is_empty()));

    workspace_audit(&gw, &fx, json!({ "mode": "on", "objects": true })).await;
    let (st, file) = upload(&gw, &fx.key, "notes.txt", "hello file").await;
    assert_eq!(st, 200, "{file}");
    let id = file["id"].as_str().unwrap().to_owned();
    let content = format!("/v1/files/{id}/content");
    let r = anthropic(&gw, reqwest::Method::GET, &content, &fx.key).send().await.unwrap();
    assert_eq!(r.text().await.unwrap(), "hello file", "a recorded download streams as before");
    let (st, _) = answer(anthropic(&gw, reqwest::Method::GET, "/v1/files", &fx.key)).await;
    assert_eq!(st, 200);
    let (st, created) = answer(
        anthropic(&gw, reqwest::Method::POST, "/v1/messages/batches", &fx.key)
            .json(&batch(&[("ok-1", "anthropic/priced")])),
    )
    .await;
    assert_eq!(st, 200, "{created}");

    let records = sink.wait_audit(4).await;
    let text = serde_json::to_string(&records).unwrap();
    assert!(!text.contains(common::PROVIDER_SECRET) && !text.contains(&fx.key), "no credential in a record");
    // The events ship from a spool of their own: two calls unrecorded, four recorded.
    let events = sink.wait_events(6).await;
    for r in &records {
        assert!(events.iter().any(|e| e.audit_id == r.audit_id), "each event names its record");
    }

    // The upload as it streamed through: the provider got the same bytes.
    let up = record_of(&records, "POST", "/v1/files");
    assert!(up.client_request.body.contains("hello file"), "{}", up.client_request.body);
    assert!(!up.client_request.truncated);
    assert_eq!(up.provider_requests[0].same_as, "client_request");
    assert_eq!((up.provider_responses[0].status, up.client_response.same_as.as_str()), (200, "provider_responses[0]"));

    // The download, all of it, and its event as an unrecorded download's.
    let down = record_of(&records, "GET", &content);
    assert_eq!(down.provider_responses[0].body, "hello file");
    assert_eq!(down.client_response.same_as, "provider_responses[0]");
    let recorded = events.iter().find(|e| e.audit_id == down.audit_id).unwrap();
    let before = plain.iter().find(|e| e.path == "/v1/files/{id}/content").unwrap();
    assert_eq!(
        (recorded.status.as_str(), recorded.http_status, recorded.usage_source.as_str()),
        (before.status.as_str(), before.http_status, before.usage_source.as_str())
    );

    // A list the gateway answers from its record: no provider, what the caller got.
    let list = record_of(&records, "GET", "/v1/files");
    assert!(list.provider_requests.is_empty() && list.client_response.body.contains(&id));

    // A batch: what was asked, what was sent (the provider's model id), and both answers.
    let made = record_of(&records, "POST", "/v1/messages/batches");
    assert!(made.client_request.body.contains("anthropic/priced"));
    let sent = &made.provider_requests[0].body;
    assert!(sent.contains("\"priced\"") && !sent.contains("anthropic/priced"), "{sent}");
    // The caller's answer, stored once where it is the provider's.
    let got = match made.client_response.same_as.as_str() {
        "" => &made.client_response.body,
        _ => &made.provider_responses[0].body,
    };
    assert!(got.contains(created["id"].as_str().unwrap()), "{got}");
}

#[tokio::test]
async fn file_contents_are_kept_as_size_and_hash_in_reference_mode() {
    let (gw, _mock, sink, fx, _) = setup().await;
    workspace_audit(&gw, &fx, json!({ "mode": "on", "objects": true, "attachments": "reference" })).await;
    let (st, file) = upload(&gw, &fx.key, "photo.bin", "hello file").await;
    assert_eq!(st, 200, "{file}");
    let content = format!("/v1/files/{}/content", file["id"].as_str().unwrap());
    let r = anthropic(&gw, reqwest::Method::GET, &content, &fx.key).send().await.unwrap();
    assert_eq!(r.text().await.unwrap(), "hello file");

    let records = sink.wait_audit(2).await;
    let sha = hex::encode(ring::digest::digest(&ring::digest::SHA256, b"hello file"));
    let up = record_of(&records, "POST", "/v1/files");
    let form: Value = serde_json::from_str(&up.client_request.body).unwrap();
    assert_eq!(
        form["form"][0],
        json!({ "name": "file", "filename": "photo.bin", "content_type": "text/plain", "bytes": 10, "sha256": sha })
    );
    assert!(!up.client_request.truncated, "a summary is not the body cut short");
    assert!(up.client_request.body_bytes > 10, "the size is the form's");
    let down = record_of(&records, "GET", &content);
    assert_eq!(down.provider_responses[0].body, format!("[attachment: 10 bytes, sha256 {sha}]"));
    assert!(!down.truncated && down.client_response.same_as == "provider_responses[0]");
}

#[tokio::test]
async fn a_file_call_that_must_be_recorded_is_refused_while_records_cannot_be_stored() {
    // No analytics service: there is no audit spool to write to.
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(None).await;
    let fx = gw.fixture("anthropic", &mock.base_url()).await;
    workspace_audit(&gw, &fx, json!({ "mode": "on", "objects": true, "guarantee": "required" })).await;
    let r = anthropic(&gw, reqwest::Method::GET, "/v1/files", &fx.key).send().await.unwrap();
    assert_eq!((r.status().as_u16(), r.headers()["x-gw-error-type"].to_str().unwrap()), (503, "audit_unavailable"));
    let (st, _) = upload(&gw, &fx.key, "notes.txt", "hello file").await;
    assert_eq!(st, 503);
    assert!(mock.captured().iter().all(|c| c.path != "/v1/files"), "nothing reached the provider");
}
