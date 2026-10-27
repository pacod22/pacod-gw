//! Benchmark reports through the management API (docs/testing.md, section 10): kept as sent,
//! once per digest, refused with the problem named, for platform admins only, and no change to
//! the configuration.

mod common;

use common::TestGw;
use serde_json::{Value, json};

fn fixture(name: &str) -> Vec<u8> {
    let path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../pacod-gw-bench-report/tests/fixtures/v1").join(name);
    std::fs::read(path).unwrap()
}

async fn post(gw: &TestGw, token: &str, body: Vec<u8>, upload: bool) -> (u16, Value) {
    let mut req = gw
        .http
        .post(format!("{}/api/v1/benchmark-reports", gw.admin))
        .bearer_auth(token)
        .header("content-type", "application/json")
        .body(body);
    if upload {
        req = req.header("x-gw-bench-source", "upload");
    }
    let r = req.send().await.unwrap();
    let status = r.status().as_u16();
    (status, r.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn a_report_is_kept_once_as_sent_and_read_back() {
    let gw = TestGw::start(None).await;
    let (_, before) = gw.api("GET", "/system/status", None).await;
    let full = fixture("full.json");
    let (st, kept) = post(&gw, &gw.token, full.clone(), false).await;
    assert_eq!(st, 201, "{kept}");
    let s = &kept["summary"];
    assert_eq!((s["source"].as_str(), s["measurement_class"].as_str()), (Some("push"), Some("behaviour")));
    assert_eq!(
        (s["gateway_version"].as_str(), s["layout"].as_str(), s["cells"].as_i64()),
        (Some("0.1.0"), Some("single"), Some(3))
    );
    assert_eq!(s["published_by_name"], "admin@example.test", "the caller");
    assert_eq!(s["run_by"], "admin@example.com", "as the report says");
    let id = s["id"].as_str().unwrap().to_owned();

    // The same bytes again: kept once.
    let (st, again) = post(&gw, &gw.token, full.clone(), true).await;
    assert_eq!((st, again["created"].as_bool()), (200, Some(false)));
    assert_eq!(again["summary"]["id"], id.as_str());

    // A newer minor version with fields this gateway does not know: kept, and read back whole.
    let (st, newer) = post(&gw, &gw.token, fixture("unknown-fields.json"), true).await;
    assert_eq!(st, 201, "{newer}");
    assert_eq!(newer["summary"]["source"], "upload");
    let (_, view) =
        gw.api("GET", &format!("/benchmark-reports/{}", newer["summary"]["id"].as_str().unwrap()), None).await;
    assert_eq!(view["document"]["a_later_field"]["x"], 1, "the stored bytes keep what this version does not know");

    let (st, list) = gw.api("GET", "/benchmark-reports", None).await;
    assert_eq!(st, 200);
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    assert_eq!(list["items"][1]["id"], id.as_str(), "newest first");

    let (_, view) = gw.api("GET", &format!("/benchmark-reports/{id}"), None).await;
    let sent: Value = serde_json::from_slice(&full).unwrap();
    assert_eq!(view["document"], sent);
    let raw = gw
        .http
        .get(format!("{}/api/v1/benchmark-reports/{id}/document", gw.admin))
        .bearer_auth(&gw.token)
        .send()
        .await
        .unwrap();
    assert!(raw.headers()["content-disposition"].to_str().unwrap().contains(&id));
    assert_eq!(raw.bytes().await.unwrap().as_ref(), full.as_slice(), "byte for byte as it was sent");

    // Reports are not configuration.
    let (_, after) = gw.api("GET", "/system/status", None).await;
    assert_eq!(before["config_head"], after["config_head"]);
    let (_, log) = gw.api("GET", "/audit-log", None).await;
    assert!(log.to_string().contains("keep_benchmark_report"), "{log}");

    let (st, _) = gw.api("DELETE", &format!("/benchmark-reports/{id}"), None).await;
    assert_eq!(st, 204);
    let (st, _) = gw.api("GET", &format!("/benchmark-reports/{id}"), None).await;
    assert_eq!(st, 404);
}

#[tokio::test]
async fn a_report_that_does_not_pass_is_refused_with_its_problems() {
    let gw = TestGw::start(None).await;
    let (st, e) = post(&gw, &gw.token, fixture("bad-histogram.json"), true).await;
    assert_eq!(st, 422, "{e}");
    assert!(e["error"]["details"]["problems"].to_string().contains("cells[0].histograms.total"), "{e}");
    let (st, _) = post(&gw, &gw.token, fixture("wrong-schema.json"), true).await;
    assert_eq!(st, 422);
    let mut long = fixture("full.json");
    long.extend(std::iter::repeat_n(b' ', pacod_gw_bench_report::MAX_BYTES));
    let (st, _) = post(&gw, &gw.token, long, true).await;
    assert_eq!(st, 413);
}

#[tokio::test]
async fn only_platform_admins_keep_and_read_reports() {
    let gw = TestGw::start(None).await;
    let made = gw.create("/admin-tokens", json!({ "name": "a-viewer" })).await;
    gw.create("/role-bindings", json!({ "principal": made.id, "role": "viewer", "scope": { "type": "platform" } }))
        .await;
    let viewer = made.body["secret"].as_str().unwrap().to_owned();
    let (st, _) = post(&gw, &viewer, fixture("full.json"), false).await;
    assert_eq!(st, 403);
    let (st, _) = gw.api_as(&viewer, "GET", "/benchmark-reports", None).await;
    assert_eq!(st, 403);
}
