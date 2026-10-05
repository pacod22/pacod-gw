//! The control endpoints, the lifecycle of the handle, and the `gw-mock` binary.

use std::{process::Stdio, time::Duration};

use pacod_gw_testkit::mock::{CapturedRequest, MockProvider};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

use crate::support::{client, get, post};

const CHAT: &str = "/v1/chat/completions";

fn chat() -> Value {
    json!({ "model": "gpt-4o", "messages": [{ "role": "user", "content": "hi" }] })
}

/// Exercises the control endpoints of a mock at `base`, as a test against the container would.
async fn control_round_trip(base: &str) {
    let http = client();
    let health = http.get(format!("{base}/healthz")).send().await.unwrap();
    assert_eq!(health.status(), 200);
    assert_eq!(health.json::<Value>().await.unwrap(), json!({ "status": "ok" }));

    // Queue two behaviours; provider requests use them up in order.
    let queued = http
        .post(format!("{base}/__mock/script"))
        .json(&json!(["status=503,retry_after=2", "model=scripted"]))
        .send()
        .await
        .unwrap();
    assert_eq!(queued.status(), 200);
    assert_eq!(queued.json::<Value>().await.unwrap(), json!({ "queued": 2 }));
    let chat_url = format!("{base}{CHAT}");
    let body = serde_json::to_vec(&chat()).unwrap();
    let send_chat = async || {
        http.post(&chat_url).header("authorization", "Bearer sk-test").body(body.clone()).send().await.unwrap()
    };
    let failed = send_chat().await;
    assert_eq!(failed.status(), 503);
    assert_eq!(failed.headers()["retry-after"], "2");
    let served: Value = send_chat().await.json().await.unwrap();
    assert_eq!(served["model"], "scripted");
    let plain: Value = send_chat().await.json().await.unwrap();
    assert_eq!(plain["model"], "gpt-4o");

    // The captured requests, with bodies in base64. Control requests are not among them.
    let captured: Vec<CapturedRequest> =
        http.get(format!("{base}/__mock/requests")).send().await.unwrap().json().await.unwrap();
    assert_eq!(captured.len(), 3);
    for request in &captured {
        assert_eq!(request.method, "POST");
        assert_eq!(request.path, CHAT);
        assert_eq!(request.header("authorization"), Some("Bearer sk-test"));
        assert_eq!(&request.body[..], &body[..]);
    }

    let cleared = http.delete(format!("{base}/__mock/requests")).send().await.unwrap();
    assert_eq!(cleared.status(), 204);
    let captured: Value = http.get(format!("{base}/__mock/requests")).send().await.unwrap().json().await.unwrap();
    assert_eq!(captured, json!([]));

    // A queued script can be dropped again.
    http.post(format!("{base}/__mock/script")).json(&json!(["status=500"])).send().await.unwrap();
    let dropped = http.delete(format!("{base}/__mock/script")).send().await.unwrap();
    assert_eq!(dropped.status(), 204);
    assert_eq!(send_chat().await.status(), 200);
}

#[tokio::test]
async fn control_endpoints() {
    let mock = MockProvider::spawn().await;
    control_round_trip(&mock.base_url()).await;
    // What the endpoints showed is what the handle shows.
    assert_eq!(mock.captured().len(), 1);
    assert_eq!(mock.pending_script(), 0);
}

#[tokio::test]
async fn control_endpoints_reject_misuse() {
    let mock = MockProvider::spawn().await;
    for body in ["not json", r#"{"script":[]}"#, r#"["bogus=1"]"#] {
        let response = post(&mock, "/__mock/script").body(body).send().await.unwrap();
        assert_eq!(response.status(), 400, "{body}");
        let error: Value = response.json().await.unwrap();
        assert_eq!(error["error"]["type"], "mock_error");
    }
    assert_eq!(mock.pending_script(), 0);
    let response = get(&mock, "/__mock/nope").send().await.unwrap();
    assert_eq!(response.status(), 404);
    assert!(mock.captured().is_empty());
}

#[tokio::test]
async fn the_handle_and_the_endpoints_share_state() {
    let mock = MockProvider::spawn().await;
    assert!(mock.base_url().starts_with("http://127.0.0.1:"));
    assert_eq!(mock.base_url(), format!("http://{}", mock.addr()));

    post(&mock, "/__mock/script").json(&json!(["model=a", "model=b"])).send().await.unwrap();
    assert_eq!(mock.pending_script(), 2);
    post(&mock, CHAT).json(&chat()).send().await.unwrap();
    assert_eq!(mock.pending_script(), 1);

    let listed: Vec<CapturedRequest> = get(&mock, "/__mock/requests").send().await.unwrap().json().await.unwrap();
    assert_eq!(listed, mock.captured());
}

#[tokio::test]
async fn shutdown_releases_the_port() {
    let mock = MockProvider::spawn().await;
    let addr = mock.addr();
    assert_eq!(get(&mock, "/healthz").send().await.unwrap().status(), 200);
    mock.shutdown().await;
    assert!(tokio::net::TcpStream::connect(addr).await.is_err());

    // Dropping stops the server too, a moment later.
    let mock = MockProvider::spawn().await;
    let addr = mock.addr();
    drop(mock);
    let mut refused = false;
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(addr).await.is_err() {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(refused, "the mock still accepts connections after drop");
}

#[tokio::test]
async fn binary_serves_the_same_mock() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_gw-mock"))
        .args(["--listen", "127.0.0.1:0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let announced = tokio::time::timeout(Duration::from_secs(20), lines.next_line())
        .await
        .expect("gw-mock announces its address")
        .unwrap()
        .unwrap();
    let base =
        announced.strip_prefix("gw-mock listening on ").unwrap_or_else(|| panic!("unexpected first line: {announced}"));
    assert!(base.starts_with("http://127.0.0.1:"));

    control_round_trip(base).await;
    child.kill().await.unwrap();
}
