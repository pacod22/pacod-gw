//! Proxy mode relays WebSocket sessions byte for byte: the handshake with the provider's
//! credential instead of the caller's key, the frames both ways, one event per session.

mod common;

use common::{Fixture, IngestSink, PROVIDER_SECRET, TestGw};
use futures_util::{SinkExt, StreamExt};
use pacod_gw_testkit::mock::MockProvider;
use serde_json::json;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

async fn setup() -> (TestGw, MockProvider, IngestSink, Fixture) {
    let sink = IngestSink::start().await;
    let mock = MockProvider::spawn().await;
    let gw = TestGw::start(Some(&sink)).await;
    let fx = gw.fixture("openai", &format!("{}/v1", mock.base_url())).await;
    (gw, mock, sink, fx)
}

/// Opens a WebSocket to the gateway at `path`, with `headers`.
async fn connect(
    gw: &TestGw,
    path: &str,
    headers: &[(&str, &str)],
) -> Result<
    (
        tokio_tungstenite::WebSocketStream<tokio::net::TcpStream>,
        tokio_tungstenite::tungstenite::handshake::client::Response,
    ),
    tokio_tungstenite::tungstenite::Error,
> {
    let addr = gw.data.trim_start_matches("http://");
    let mut req = format!("ws://{addr}{path}").into_client_request().unwrap();
    for (k, v) in headers {
        let name = http::HeaderName::from_bytes(k.as_bytes()).unwrap();
        req.headers_mut().insert(name, v.parse().unwrap());
    }
    let tcp = tokio::net::TcpStream::connect(addr).await.unwrap();
    tokio_tungstenite::client_async(req, tcp).await
}

#[tokio::test]
async fn a_session_is_relayed_with_the_providers_credential() {
    let (gw, mock, sink, fx) = setup().await;
    let bearer = format!("Bearer {}", fx.key);
    let (mut ws, response) = connect(&gw, "/proxy/openai/__ws/echo?model=m", &[("authorization", &bearer)])
        .await
        .expect("the session opens");
    assert!(response.headers().get("x-gw-request-id").is_some());
    ws.send(Message::text("hello")).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap(), Message::text("hello"));
    ws.send(Message::binary(vec![1u8, 2, 3])).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap(), Message::binary(vec![1u8, 2, 3]));
    ws.close(None).await.unwrap();

    // The provider saw its own credential, never the caller's key, and the caller's handshake.
    let sent = mock.captured().into_iter().find(|c| c.path.starts_with("/__ws/echo")).unwrap();
    assert_eq!(sent.header("authorization"), Some(format!("Bearer {PROVIDER_SECRET}").as_str()));
    assert!(!format!("{:?}", sent.headers).contains(&fx.key), "the gateway key never reaches the provider");
    assert_eq!(sent.header("upgrade"), Some("websocket"));
    assert!(sent.header("sec-websocket-key").is_some());

    let e = sink.wait_events(1).await.remove(0);
    assert_eq!(
        (e.mode.as_str(), e.path.as_str(), e.http_status, e.status.as_str()),
        ("proxy", "/__ws/echo", 101, "ok")
    );
    assert_eq!(e.model_served, "openai/m");
}

#[tokio::test]
async fn a_key_offered_as_a_subprotocol_opens_the_session_and_stays_with_the_gateway() {
    let (gw, mock, _sink, fx) = setup().await;
    let protocols = format!("realtime, openai-insecure-api-key.{}", fx.key);
    let (mut ws, response) = connect(&gw, "/proxy/openai/__ws/echo?model=m", &[("sec-websocket-protocol", &protocols)])
        .await
        .expect("opens");
    assert_eq!(response.headers()["sec-websocket-protocol"], "realtime");
    ws.send(Message::text("x")).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap(), Message::text("x"));
    let sent = mock.captured().into_iter().find(|c| c.path.starts_with("/__ws/echo")).unwrap();
    assert_eq!(sent.header("sec-websocket-protocol"), Some("realtime"));
}

#[tokio::test]
async fn sessions_that_cannot_be_accounted_or_checked_are_refused() {
    let (gw, mock, _sink, fx) = setup().await;
    let bearer = format!("Bearer {}", fx.key);
    // No model on the shared credential: it would reach whatever the credential reaches.
    let e = connect(&gw, "/proxy/openai/__ws/echo", &[("authorization", &bearer)]).await.unwrap_err();
    assert!(matches!(e, tokio_tungstenite::tungstenite::Error::Http(ref r) if r.status() == 403), "{e:?}");
    // A key that requires usage: a relayed session reports none.
    let strict = gw.key(&fx, json!({ "name": "strict", "usage_policy": "require" })).await;
    let e = connect(&gw, "/proxy/openai/__ws/echo?model=m", &[("authorization", &format!("Bearer {strict}"))])
        .await
        .unwrap_err();
    assert!(matches!(e, tokio_tungstenite::tungstenite::Error::Http(ref r) if r.status() == 403), "{e:?}");
    // A provider that refuses the handshake: its answer is the caller's.
    let e = connect(&gw, "/proxy/openai/__ws/none?model=m", &[("authorization", &bearer)]).await.unwrap_err();
    assert!(matches!(e, tokio_tungstenite::tungstenite::Error::Http(ref r) if r.status() == 404), "{e:?}");
    assert!(mock.captured().iter().all(|c| !c.path.starts_with("/__ws/echo")), "nothing was opened");
    // An ephemeral key of the Realtime API is not made on the shared credential.
    let r = gw
        .http
        .post(format!("{}/proxy/openai/v1/realtime/sessions", gw.data))
        .bearer_auth(&fx.key)
        .json(&json!({ "model": "m" }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
}

#[tokio::test]
async fn a_stopping_instance_closes_its_sessions_and_records_them() {
    let (gw, _mock, sink, fx) = setup().await;
    let bearer = format!("Bearer {}", fx.key);
    let (mut ws, _) = connect(&gw, "/proxy/openai/__ws/echo?model=m", &[("authorization", &bearer)]).await.unwrap();
    ws.send(Message::text("before")).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap(), Message::text("before"));
    assert_eq!(gw.app.gw.sessions.tracker.len(), 1);
    gw.app.gw.sessions.drain(std::time::Duration::from_secs(5)).await;
    assert!(gw.app.gw.sessions.tracker.is_empty(), "the session has ended");
    // The caller's end sees the connection close.
    let next = tokio::time::timeout(std::time::Duration::from_secs(2), ws.next()).await.unwrap();
    assert!(!matches!(next, Some(Ok(Message::Text(_)))), "{next:?}");
    let e = sink.wait_events(1).await.remove(0);
    assert_eq!((e.status.as_str(), e.error_type.as_str()), ("ok", "instance_stopping"));
}

async fn workspace_audit(gw: &TestGw, fx: &Fixture, audit: serde_json::Value) {
    let path = format!("/orgs/{}/workspaces/{}", fx.org, fx.ws);
    let settings = json!({ "name": "prod", "tag_keys": ["purpose", "site"], "settings": { "audit": audit } });
    let (st, v) = gw.api("PUT", &path, Some(settings)).await;
    assert_eq!(st, 200, "{v}");
    gw.wait_applied().await;
}

fn refused_with(e: &tokio_tungstenite::tungstenite::Error) -> (u16, String) {
    match e {
        tokio_tungstenite::tungstenite::Error::Http(r) => (
            r.status().as_u16(),
            r.headers().get("x-gw-error-type").map(|v| v.to_str().unwrap().to_owned()).unwrap_or_default(),
        ),
        other => panic!("not refused over HTTP: {other:?}"),
    }
}

#[tokio::test]
async fn a_workspace_that_refuses_what_it_cannot_record_refuses_relayed_sessions() {
    let (gw, mock, sink, fx) = setup().await;
    let bearer = format!("Bearer {}", fx.key);
    let auth = ("authorization", bearer.as_str());
    workspace_audit(&gw, &fx, json!({ "mode": "on", "realtime": true, "unrecordable": "refuse" })).await;
    let e = connect(&gw, "/proxy/openai/__ws/echo?model=m", &[auth]).await.unwrap_err();
    assert_eq!(refused_with(&e), (403, "not_allowed".into()));
    assert!(mock.captured().iter().all(|c| !c.path.starts_with("/__ws/echo")), "nothing was opened");

    // On request: only a session that asks to be recorded is one audit mode cannot record.
    workspace_audit(&gw, &fx, json!({ "mode": "on_request", "realtime": true, "unrecordable": "refuse" })).await;
    let (mut ws, _) = connect(&gw, "/proxy/openai/__ws/echo?model=m", &[auth]).await.expect("opens");
    ws.close(None).await.unwrap();
    let e = connect(&gw, "/proxy/openai/__ws/echo?model=m", &[auth, ("x-gw-audit", "true")]).await.unwrap_err();
    assert_eq!(refused_with(&e).0, 403);

    // Allowed unless set otherwise: served without a record. Gateway headers a relayed session
    // does not read change nothing.
    workspace_audit(&gw, &fx, json!({ "mode": "on", "realtime": true })).await;
    let (mut ws, _) =
        connect(&gw, "/proxy/openai/__ws/echo?model=m", &[auth, ("x-gw-complexity", "extreme")]).await.expect("opens");
    ws.send(Message::text("hello")).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap(), Message::text("hello"));
    ws.close(None).await.unwrap();
    let events = sink.wait_events(4).await;
    assert!(events.iter().all(|e| e.audit_id.is_empty()));
    assert!(sink.audit_records().is_empty());
}

#[tokio::test]
async fn sessions_opened_at_the_provider_are_refused_where_they_cannot_be_recorded() {
    let (gw, mock, _sink, fx) = setup().await;
    let post = |path: &'static str| {
        let url = format!("{}{path}", gw.data);
        let key = fx.key.clone();
        let http = gw.http.clone();
        async move {
            let r = http
                .post(url)
                .bearer_auth(key)
                .header("content-type", "application/sdp")
                .body("v=0\r\n")
                .send()
                .await
                .unwrap();
            let kind = r.headers().get("x-gw-error-type").map(|v| v.to_str().unwrap().to_owned()).unwrap_or_default();
            (r.status().as_u16(), kind)
        }
    };
    // WebRTC never runs on the installation's shared credential, as ephemeral keys do not.
    for path in ["/proxy/openai/v1/realtime/calls", "/proxy/openai/v1/realtime?model=m"] {
        assert_eq!(post(path).await, (403, "own_credential_required".into()), "{path}");
    }

    gw.create(
        &format!("/providers/{}/credentials", fx.provider),
        json!({ "label": "own", "secret": "sk-own", "scope": { "type": "workspace", "id": fx.ws } }),
    )
    .await;
    workspace_audit(&gw, &fx, json!({ "mode": "on", "realtime": true, "unrecordable": "refuse" })).await;
    for path in ["/proxy/openai/v1/realtime/client_secrets", "/proxy/openai/v1/realtime/calls"] {
        assert_eq!(post(path).await, (403, "not_allowed".into()), "{path}");
    }
    assert!(mock.captured().iter().all(|c| !c.path.contains("/realtime")), "nothing reached the provider");

    // Allowed, they are relayed on the workspace's own credential.
    workspace_audit(&gw, &fx, json!({ "mode": "on", "realtime": true })).await;
    let _ = post("/proxy/openai/v1/realtime/client_secrets").await;
    assert!(mock.captured().iter().any(|c| c.path.starts_with("/v1/realtime/client_secrets")));
}
