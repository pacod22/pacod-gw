//! Request capture: what the mock received, byte for byte.

use pacod_gw_testkit::mock::{CapturedRequest, MockProvider};
use serde_json::json;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
};

use crate::support::{anthropic_post, get, post};

#[tokio::test]
async fn bodies_are_captured_byte_for_byte() {
    let mock = MockProvider::spawn().await;

    // Valid JSON that no serialiser would produce: odd spacing, escapes, a trailing newline.
    let odd =
        "{ \"model\" :\"gpt-4o\",\n\t\"messages\":[{\"role\":\"user\",\"content\":\"h\\u00e9llo \\/ w\u{f6}rld\"}] }\n";
    let response = post(&mock, "/v1/chat/completions?api-version=2024-10-21&x=a%20b")
        .header("content-type", "application/json")
        .body(odd)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);

    // Not JSON, not even text: rejected, and captured all the same.
    let garbage: &[u8] = b"\xff\x00\x01{not json\r\n";
    let response = anthropic_post(&mock, "/v1/messages").body(garbage).send().await.unwrap();
    assert_eq!(response.status(), 400);

    let response = get(&mock, "/v1/models").send().await.unwrap();
    assert_eq!(response.status(), 200);

    let captured = mock.captured();
    assert_eq!(captured.len(), 3);
    assert_eq!(captured[0].method, "POST");
    assert_eq!(captured[0].path, "/v1/chat/completions?api-version=2024-10-21&x=a%20b");
    assert_eq!(&captured[0].body[..], odd.as_bytes());
    assert_eq!(captured[0].header("Content-Type"), Some("application/json"));
    assert_eq!(captured[0].json().unwrap()["messages"][0]["content"], "h\u{e9}llo / w\u{f6}rld");

    assert_eq!(captured[1].path, "/v1/messages");
    assert_eq!(&captured[1].body[..], garbage);
    assert_eq!(captured[1].header("x-api-key"), Some("sk-ant-test"));
    assert!(captured[1].json().is_err());

    assert_eq!(captured[2].method, "GET");
    assert_eq!(captured[2].path, "/v1/models");
    assert!(captured[2].body.is_empty());

    mock.clear();
    assert!(mock.captured().is_empty());
}

#[tokio::test]
async fn headers_are_captured_in_the_order_received() {
    let mock = MockProvider::spawn().await;
    let body = r#"{"model":"gpt-4o","messages":[{"role":"user","content":"hi"}]}"#;
    // Written by hand so that the order and the spelling on the wire are known exactly.
    let request = format!(
        "POST /v1/chat/completions?trace=1 HTTP/1.1\r\n\
         Host: provider.test\r\n\
         X-Zeta: last-letter-first\r\n\
         Authorization: Bearer sk-test\r\n\
         Accept: application/json\r\n\
         Content-Type: application/json\r\n\
         Accept: text/event-stream\r\n\
         X-Alpha: first-letter-last\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    let mut socket = TcpStream::connect(mock.addr()).await.unwrap();
    socket.write_all(request.as_bytes()).await.unwrap();
    let mut response = Vec::new();
    socket.read_to_end(&mut response).await.unwrap();
    assert!(response.starts_with(b"HTTP/1.1 200 OK\r\n"));

    let captured = mock.captured();
    assert_eq!(captured.len(), 1);
    let headers: Vec<(&str, &str)> =
        captured[0].headers.iter().map(|(name, value)| (name.as_str(), value.as_str())).collect();
    // Names arrive lower-cased. A repeated name keeps one entry per value, at the position
    // of its first occurrence: the second `Accept` was sent after `Content-Type`.
    assert_eq!(
        headers,
        [
            ("host", "provider.test"),
            ("x-zeta", "last-letter-first"),
            ("authorization", "Bearer sk-test"),
            ("accept", "application/json"),
            ("accept", "text/event-stream"),
            ("content-type", "application/json"),
            ("x-alpha", "first-letter-last"),
            ("content-length", "62"),
            ("connection", "close"),
        ]
    );
    assert_eq!(captured[0].path, "/v1/chat/completions?trace=1");
    assert_eq!(&captured[0].body[..], body.as_bytes());
}

#[tokio::test]
async fn capture_limit_keeps_the_most_recent_requests() {
    let mock = MockProvider::spawn().await;
    mock.set_capture_limit(2);
    for path in ["/one", "/two", "/three"] {
        get(&mock, path).send().await.unwrap();
    }
    let paths: Vec<String> = mock.captured().into_iter().map(|r| r.path).collect();
    assert_eq!(paths, ["/two", "/three"]);

    mock.set_capture_limit(0);
    get(&mock, "/four").send().await.unwrap();
    assert_eq!(mock.captured().len(), 2);
}

#[tokio::test]
async fn captured_requests_round_trip_through_json() {
    let request = CapturedRequest {
        method: "POST".into(),
        path: "/v1/messages".into(),
        headers: vec![("accept".into(), "a".into()), ("accept".into(), "b".into())],
        body: b"\xff\x00binary".as_slice().into(),
    };
    let value = serde_json::to_value(&request).unwrap();
    assert_eq!(
        value,
        json!({
            "method": "POST",
            "path": "/v1/messages",
            "headers": [["accept", "a"], ["accept", "b"]],
            "body_base64": "/wBiaW5hcnk=",
        })
    );
    let back: CapturedRequest = serde_json::from_value(value).unwrap();
    assert_eq!(back, request);
}
