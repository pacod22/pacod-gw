//! Managed OpenAI Realtime (`GET /v1/realtime?model=…`): a WebSocket session the gateway ends
//! on both sides, one priced event per response, the limits checked as responses come.

mod common;

use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use futures_util::{SinkExt, StreamExt};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::{Value, json};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;

type Ws = WebSocketStream<tokio::net::TcpStream>;

/// A realtime model priced by text, audio and cached tokens.
async fn setup() -> (TestGw, MockProvider, IngestSink, Fixture) {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    gw.priced_model(
        &fx,
        "openai/realtime",
        "gpt-realtime-mock",
        json!({ "kind": "realtime", "capability_preset": "realtime" }),
        json!({
            "currency": "USD", "input": 4.0, "cache_read": 0.4, "input_audio": 32.0,
            "output": 16.0, "output_audio": 64.0,
        }),
    )
    .await;
    (gw, mock, sink, fx)
}

/// What the mock's response costs: 20 text and 60 audio tokens in, 40 cached, 20 text and 40
/// audio tokens out.
const RESPONSE_COST: f64 = (20.0 * 4.0 + 60.0 * 32.0 + 40.0 * 0.4 + 20.0 * 16.0 + 40.0 * 64.0) / 1e6;

async fn connect(
    gw: &TestGw,
    path: &str,
    headers: &[(&str, &str)],
) -> Result<(Ws, tokio_tungstenite::tungstenite::handshake::client::Response), tokio_tungstenite::tungstenite::Error> {
    let addr = gw.data.trim_start_matches("http://");
    let mut req = format!("ws://{addr}{path}").into_client_request().unwrap();
    for (k, v) in headers {
        req.headers_mut().insert(http::HeaderName::from_bytes(k.as_bytes()).unwrap(), v.parse().unwrap());
    }
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    tokio_tungstenite::client_async(req, tcp).await
}

async fn open(gw: &TestGw, key: &str) -> Ws {
    let bearer = format!("Bearer {key}");
    let (ws, _) =
        connect(gw, "/v1/realtime?model=openai/realtime", &[("authorization", &bearer)]).await.expect("opens");
    ws
}

/// Reads events until one of type `kind`, and returns it.
async fn until(ws: &mut Ws, kind: &str) -> Value {
    let wait = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match ws.next().await.expect("open").expect("a frame") {
                Message::Text(t) => {
                    let v: Value = serde_json::from_str(&t).unwrap();
                    if v["type"] == kind {
                        return v;
                    }
                }
                Message::Close(f) => panic!("closed while waiting for {kind}: {f:?}"),
                _ => {}
            }
        }
    });
    wait.await.unwrap_or_else(|_| panic!("no {kind} within 5 s"))
}

async fn send(ws: &mut Ws, event: Value) {
    ws.send(Message::text(event.to_string())).await.unwrap();
}

fn http_status(e: &tokio_tungstenite::tungstenite::Error) -> u16 {
    match e {
        tokio_tungstenite::tungstenite::Error::Http(r) => r.status().as_u16(),
        other => panic!("not refused over HTTP: {other:?}"),
    }
}

#[tokio::test]
async fn each_response_is_one_priced_event_of_the_session() {
    let (gw, mock, sink, fx) = setup().await;
    let bearer = format!("Bearer {}", fx.key);
    let (mut ws, response) = connect(&gw, "/v1/realtime?model=openai/realtime", &[("authorization", &bearer)])
        .await
        .expect("the session opens");
    let request_id = response.headers()["x-gw-request-id"].to_str().unwrap().to_owned();
    assert_eq!(response.headers()["x-gw-model-served"], "openai/realtime");
    let created = until(&mut ws, "session.created").await;
    assert_eq!(created["session"]["model"], "gpt-realtime-mock");
    send(&mut ws, json!({ "type": "session.update", "session": { "instructions": "be brief" } })).await;
    until(&mut ws, "session.updated").await;
    send(&mut ws, json!({ "type": "response.create" })).await;
    assert_eq!(until(&mut ws, "response.output_text.delta").await["delta"], "Hello from the mock");
    until(&mut ws, "response.done").await;
    // A response the provider starts on its own, as its voice activity detection does.
    send(&mut ws, json!({ "type": "input_audio_buffer.commit" })).await;
    until(&mut ws, "response.done").await;
    ws.close(None).await.unwrap();

    let mut events = sink.wait_events(2).await;
    events.sort_by(|a, b| a.upstream_request_id.cmp(&b.upstream_request_id));
    for (e, id) in events.iter().zip(["resp_mock_1", "resp_mock_2"]) {
        assert_eq!(
            (e.mode.as_str(), e.interface.as_str(), e.model_served.as_str(), e.status.as_str()),
            ("managed", "realtime", "openai/realtime", "ok")
        );
        assert_eq!(e.upstream_request_id, id);
        assert_eq!(e.usage_source, "provider");
        let u = &e.usage;
        assert_eq!((u.input_tokens, u.cache_read_tokens, u.audio_input_tokens), (80, 40, 60));
        assert_eq!((u.output_tokens, u.audio_output_tokens), (60, 40));
        assert!((e.cost.unwrap() - RESPONSE_COST).abs() < 1e-12, "{:?}", e.cost);
    }
    assert!(!events[0].session_id.is_empty());
    assert_eq!(events[0].session_id, events[1].session_id, "the responses belong to one session");
    assert_ne!(events[0].id, events[1].id);
    // The handshake's request id names the first response's event, and the session.
    assert_eq!(format!("gw_{}", events[0].id.simple()), request_id);
    assert_eq!(events[0].session_id, request_id);
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(sink.events().len(), 2, "the session writes no event of its own beside its responses");

    // The provider was opened with its own credential and the provider's id of the model.
    let sent = mock.captured().into_iter().find(|c| c.path.starts_with("/v1/realtime")).unwrap();
    assert_eq!(sent.path, "/v1/realtime?model=gpt-realtime-mock");
    assert_eq!(sent.header("authorization"), Some(format!("Bearer {PROVIDER_SECRET}").as_str()));
    assert!(!format!("{:?}", sent.headers).contains(&fx.key), "the gateway key never reaches the provider");
    assert!(sent.header("sec-websocket-extensions").is_none());
}

#[tokio::test]
async fn a_browser_offers_its_key_as_a_subprotocol() {
    let (gw, mock, sink, fx) = setup().await;
    let protocols = format!("realtime, openai-insecure-api-key.{}", fx.key);
    let (mut ws, response) =
        connect(&gw, "/v1/realtime?model=openai/realtime", &[("sec-websocket-protocol", &protocols)])
            .await
            .expect("opens");
    assert_eq!(response.headers()["sec-websocket-protocol"], "realtime");
    until(&mut ws, "session.created").await;
    ws.close(None).await.unwrap();
    let sent = mock.captured().into_iter().find(|c| c.path.starts_with("/v1/realtime")).unwrap();
    assert_eq!(sent.header("sec-websocket-protocol"), Some("realtime"));
    // A session without a response is recorded once, with nothing used.
    let e = sink.wait_events(1).await.remove(0);
    assert_eq!((e.interface.as_str(), e.status.as_str(), e.http_status), ("realtime", "ok", 101));
    assert_eq!((e.usage.input_tokens, e.usage.output_tokens), (0, 0));
}

#[tokio::test]
async fn what_cannot_hold_a_session_is_refused_before_it_opens() {
    let (gw, mock, _sink, fx) = setup().await;
    let bearer = format!("Bearer {}", fx.key);
    // A chat model is not a realtime model.
    let e = connect(&gw, "/v1/realtime?model=openai/m", &[("authorization", &bearer)]).await.unwrap_err();
    assert_eq!(http_status(&e), 400);
    let e = connect(&gw, "/v1/realtime", &[("authorization", &bearer)]).await.unwrap_err();
    assert_eq!(http_status(&e), 400);
    let e = connect(&gw, "/v1/realtime?model=openai/realtime", &[]).await.unwrap_err();
    assert_eq!(http_status(&e), 401);
    // Not a WebSocket, and the paths that hand out keys of the provider's.
    let r = gw.http.get(format!("{}/v1/realtime?model=openai/realtime", gw.data)).bearer_auth(&fx.key).send().await;
    assert_eq!(r.unwrap().status(), 400);
    let r = gw.http.post(format!("{}/v1/realtime/client_secrets", gw.data)).bearer_auth(&fx.key).send().await;
    assert_eq!(r.unwrap().status(), 404);
    assert!(mock.captured().iter().all(|c| !c.path.starts_with("/v1/realtime")), "nothing was opened");
}

#[tokio::test]
async fn a_spend_limit_used_up_closes_the_session_and_refuses_the_next() {
    let (gw, _mock, sink, fx) = setup().await;
    // Room for one response, not for two.
    gw.create(
        &format!("/orgs/{}/workspaces/{}/limits", fx.org, fx.ws),
        json!({ "name": "spend", "subject": "workspace", "metric": "spend", "window": "month", "amount": RESPONSE_COST * 1.5 }),
    )
    .await;
    let mut ws = open(&gw, &fx.key).await;
    until(&mut ws, "session.created").await;
    send(&mut ws, json!({ "type": "response.create" })).await;
    until(&mut ws, "response.done").await;
    send(&mut ws, json!({ "type": "response.create" })).await;
    until(&mut ws, "response.done").await;
    let error = until(&mut ws, "error").await;
    assert_eq!(error["error"]["code"], "quota_exceeded", "{error}");
    let close = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await.unwrap();
    match close {
        Some(Ok(Message::Close(Some(f)))) => assert_eq!(f.code, CloseCode::Policy),
        other => panic!("expected a policy close, got {other:?}"),
    }
    assert_eq!(sink.wait_events(2).await.len(), 2);
    // Nothing is left for another session.
    let bearer = format!("Bearer {}", fx.key);
    let e = connect(&gw, "/v1/realtime?model=openai/realtime", &[("authorization", &bearer)]).await.unwrap_err();
    assert_eq!(http_status(&e), 402);
}

#[tokio::test]
async fn a_caller_that_leaves_mid_response_has_it_cancelled_and_settled() {
    let (gw, mock, sink, fx) = setup().await;
    let mut ws = open(&gw, &fx.key).await;
    until(&mut ws, "session.created").await;
    send(&mut ws, json!({ "type": "response.create", "response": { "metadata": { "mock": "hold" } } })).await;
    until(&mut ws, "response.created").await;
    drop(ws);
    let e = sink.wait_events(1).await.remove(0);
    assert_eq!((e.status.as_str(), e.finish_reason.as_str()), ("cancelled", "client_cancelled"));
    assert_eq!((e.usage.input_tokens, e.usage.output_tokens, e.usage_source.as_str()), (20, 10, "provider"));
    assert!(e.cost.unwrap() > 0.0, "what the cancelled response used is paid for");
    assert!(mock.captured().iter().any(|c| c.path.starts_with("/v1/realtime")));
}

#[tokio::test]
async fn events_that_would_bypass_the_workspace_are_refused_with_an_error_event() {
    let (gw, _mock, _sink, fx) = setup().await;
    let ws_path = format!("/orgs/{}/workspaces/{}", fx.org, fx.ws);
    let (st, v) = gw
        .api(
            "PUT",
            &ws_path,
            Some(json!({ "name": "prod", "tag_keys": ["purpose", "site"], "settings": { "provider_tools": ["web_search"] } })),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    gw.wait_applied().await;
    let mut ws = open(&gw, &fx.key).await;
    until(&mut ws, "session.created").await;
    let mcp = json!({ "type": "mcp", "server_label": "x", "server_url": "https://mcp.example/sse" });
    send(&mut ws, json!({ "type": "session.update", "event_id": "e1", "session": { "tools": [mcp] } })).await;
    let error = until(&mut ws, "error").await;
    assert_eq!(
        (error["error"]["code"].as_str(), error["error"]["event_id"].as_str()),
        (Some("not_allowed"), Some("e1"))
    );
    // The model the session was opened with stays.
    send(&mut ws, json!({ "type": "session.update", "session": { "model": "gpt-other" } })).await;
    assert_eq!(until(&mut ws, "error").await["error"]["code"], "not_allowed");
    // A function the caller defines is its own.
    let function = json!({ "type": "function", "name": "lookup", "parameters": { "type": "object" } });
    send(&mut ws, json!({ "type": "session.update", "session": { "tools": [function] } })).await;
    until(&mut ws, "session.updated").await;
}

#[tokio::test]
async fn a_stopping_instance_closes_its_sessions_as_going_away() {
    let (gw, _mock, sink, fx) = setup().await;
    let mut ws = open(&gw, &fx.key).await;
    until(&mut ws, "session.created").await;
    gw.app.gw.sessions.drain(std::time::Duration::from_secs(5)).await;
    assert!(gw.app.gw.sessions.tracker.is_empty());
    match tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await.unwrap() {
        Some(Ok(Message::Close(Some(f)))) => assert_eq!(f.code, CloseCode::Away),
        other => panic!("expected a going-away close, got {other:?}"),
    }
    let e = sink.wait_events(1).await.remove(0);
    assert_eq!((e.status.as_str(), e.error_type.as_str()), ("ok", "instance_stopping"));
}
