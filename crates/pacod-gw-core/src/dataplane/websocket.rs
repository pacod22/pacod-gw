//! WebSocket sessions through the gateway: proxy mode relays a provider's WebSocket endpoint
//! byte for byte (OpenAI's Realtime API, Gemini's Live API), with the caller's key exchanged
//! for the provider's credential.
//!
//! The handshake goes to the provider on a connection of its own (HTTP/1.1, through the egress
//! policy), with the caller's `Sec-WebSocket-Key`, so that the provider's
//! `Sec-WebSocket-Accept` holds for the caller and the two ends negotiate their extensions with
//! each other. `Connection` and `Upgrade` are hop-by-hop headers, which the gateway never
//! forwards otherwise; here they are set again for the provider, and nothing else of the
//! handshake is added. Once the provider has switched, the gateway answers the caller's 101 and
//! relays bytes both ways until either end closes, the session is idle too long or lasts too
//! long, or the instance stops.
//!
//! A relayed session is not read, so it reports no usage: it is recorded as one request with
//! its bytes, and a key whose usage policy is `require` is refused (the managed
//! `/v1/realtime` reads its usage). Audit mode cannot record it either: a workspace that refuses
//! the sessions it cannot record refuses it.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::Request;
use axum::http::header::{CONNECTION, SEC_WEBSOCKET_PROTOCOL, UPGRADE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::Response;
use hyper_util::rt::TokioIo;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::Gw;
use super::auth::{Caller, authenticate};
use super::common;
use super::errors::{Envelope, GwError};
use crate::model::{AuthScheme, Mode, Status, UsagePolicy};
use crate::relay::CallRecorder;
use crate::resolve::{Target, resolve_upstream};
use crate::snapshot::CredentialOwner;
use crate::telemetry::metrics;
use crate::telemetry::trace::TraceContext;
use crate::upstream::{SendError, Upgrade};

/// How long a session may go without a byte in either direction.
pub const IDLE: Duration = Duration::from_secs(300);
/// How long a session may last at most: OpenAI's own limit for a Realtime session.
pub const MAX_SESSION: Duration = Duration::from_secs(3600);
/// How long the provider may take to answer the handshake.
pub const HANDSHAKE: Duration = Duration::from_secs(30);

/// How a browser sends a key it cannot put in a header: as a subprotocol (OpenAI's Realtime
/// convention). The gateway takes the gateway key from there and never forwards it.
const KEY_PROTOCOL: &str = "openai-insecure-api-key.";

/// The handshake headers that pass between the caller and the provider, both ways.
const HANDSHAKE_HEADERS: [&str; 5] = [
    "sec-websocket-key",
    "sec-websocket-version",
    "sec-websocket-extensions",
    "sec-websocket-protocol",
    "sec-websocket-accept",
];

/// Whether a request asks to open a WebSocket.
pub fn is_upgrade(method: &Method, headers: &HeaderMap) -> bool {
    let has = |name: HeaderName, word: &str| {
        headers
            .get_all(name)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .any(|v| v.split(',').any(|t| t.trim().eq_ignore_ascii_case(word)))
    };
    method == Method::GET && has(CONNECTION, "upgrade") && has(UPGRADE, "websocket")
}

/// Moves a gateway key the caller offered as a subprotocol into `Authorization`, and leaves
/// the other subprotocols as they were.
pub fn take_protocol_key(headers: &mut HeaderMap) {
    let offered: Vec<String> = headers
        .get_all(SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect();
    let Some(key) = offered.iter().find_map(|p| p.strip_prefix(KEY_PROTOCOL)).map(str::to_owned) else { return };
    headers.remove(SEC_WEBSOCKET_PROTOCOL);
    let rest: Vec<&str> = offered.iter().map(String::as_str).filter(|p| !p.starts_with(KEY_PROTOCOL)).collect();
    if !rest.is_empty()
        && let Ok(v) = HeaderValue::from_str(&rest.join(", "))
    {
        headers.insert(SEC_WEBSOCKET_PROTOCOL, v);
    }
    if !headers.contains_key(axum::http::header::AUTHORIZATION)
        && let Ok(mut v) = HeaderValue::from_str(&format!("Bearer {key}"))
    {
        v.set_sensitive(true);
        headers.insert(axum::http::header::AUTHORIZATION, v);
    }
}

/// The headers of the handshake to the provider: the caller's, filtered as every relayed
/// request is, with the credential, and the upgrade asked for again.
pub fn handshake_headers(incoming: &HeaderMap, credential: &common::AuthHeader) -> HeaderMap {
    let mut out = common::relayed_headers(incoming, credential, None);
    out.remove(axum::http::header::ACCEPT_ENCODING);
    out.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
    out.insert(UPGRADE, HeaderValue::from_static("websocket"));
    out
}

/// The caller's 101: the provider's handshake answer, and the gateway's own headers.
fn switching(provider: &HeaderMap) -> Response<Body> {
    let mut r = Response::new(Body::empty());
    *r.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let h = r.headers_mut();
    h.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
    h.insert(UPGRADE, HeaderValue::from_static("websocket"));
    for name in HANDSHAKE_HEADERS {
        for v in provider.get_all(name) {
            h.append(HeaderName::from_static(name), v.clone());
        }
    }
    r
}

/// Why a relayed session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ending {
    /// One of the ends closed the connection.
    Closed,
    Idle,
    TooLong,
    /// The instance stops.
    Stopping,
    Failed,
}

impl Ending {
    fn kind(self) -> &'static str {
        match self {
            Ending::Closed => "",
            Ending::Idle => "session_idle",
            Ending::TooLong => "session_too_long",
            Ending::Stopping => "instance_stopping",
            Ending::Failed => "upstream_error",
        }
    }
}

/// Relays bytes both ways until either end closes or a limit is reached. Returns the bytes
/// sent to the provider, those sent to the caller, and why it ended.
pub async fn pump<C, P>(
    client: C,
    provider: P,
    closing: tokio_util::sync::CancellationToken,
    idle: Duration,
    longest: Duration,
) -> (u64, u64, Ending)
where
    C: AsyncRead + AsyncWrite + Unpin,
    P: AsyncRead + AsyncWrite + Unpin,
{
    let (mut cr, mut cw) = tokio::io::split(client);
    let (mut pr, mut pw) = tokio::io::split(provider);
    let (mut up, mut down) = (0u64, 0u64);
    let mut from_client = vec![0u8; 16 << 10];
    let mut from_provider = vec![0u8; 16 << 10];
    let ends = tokio::time::sleep(longest);
    tokio::pin!(ends);
    let mut last = Instant::now();
    let ending = loop {
        let quiet = tokio::time::sleep_until((last + idle).into());
        tokio::select! {
            n = cr.read(&mut from_client) => match n {
                Ok(0) | Err(_) => break Ending::Closed,
                Ok(n) => {
                    if pw.write_all(&from_client[..n]).await.is_err() { break Ending::Failed }
                    up += n as u64;
                    last = Instant::now();
                }
            },
            n = pr.read(&mut from_provider) => match n {
                Ok(0) => break Ending::Closed,
                Err(_) => break Ending::Failed,
                Ok(n) => {
                    if cw.write_all(&from_provider[..n]).await.is_err() { break Ending::Closed }
                    down += n as u64;
                    last = Instant::now();
                }
            },
            _ = quiet => break Ending::Idle,
            _ = &mut ends => break Ending::TooLong,
            _ = closing.cancelled() => break Ending::Stopping,
        }
    };
    let _ = pw.shutdown().await;
    let _ = cw.shutdown().await;
    (up, down, ending)
}

fn send_error(e: SendError) -> GwError {
    match e {
        SendError::Egress(m) => GwError::not_allowed(m),
        SendError::Timeout(d) => {
            GwError::upstream_timeout(format!("the provider did not open the session within {d:?}"))
        }
        other => GwError::upstream_error(other.to_string()),
    }
}

/// What the provider answered the handshake.
enum Opened {
    Switched { head: http::response::Parts, io: hyper::upgrade::Upgraded, target: Option<Target> },
    Refused { response: http::Response<hyper::body::Incoming>, target: Option<Target> },
}

/// Proxy mode: `GET /proxy/{provider}/{path}` asking to open a WebSocket.
pub async fn proxy(gw: Arc<Gw>, client: IpAddr, provider_name: String, path: String, mut req: Request) -> Response {
    let started = Instant::now();
    // Taken before the request is taken apart: the upgrade lives in its extensions.
    let on_client = hyper::upgrade::on(&mut req);
    let (mut parts, _) = req.into_parts();
    let trace = TraceContext::from_traceparent(parts.headers.get("traceparent").and_then(|v| v.to_str().ok()));
    super::auth::take_query_key(&mut parts);
    take_protocol_key(&mut parts.headers);
    let family = gw.config.snapshot().provider_by_name(&provider_name).map(|p| p.doc.family);
    let envelope = if family == Some(crate::model::Family::Anthropic) { Envelope::Anthropic } else { Envelope::OpenAi };
    let caller = match authenticate(&gw, &parts.headers, Some(client), Mode::Proxy) {
        Ok(c) => c,
        Err(e) => {
            metrics::record_rejection(e.kind);
            gw.live.record_rejection(e.kind);
            gw.live.record_rejected();
            return e.response(envelope, "");
        }
    };
    let mut rec = CallRecorder::new(gw.clone(), trace);
    rec.timeline = crate::relay::Timeline::starting_at(started);
    rec.event.organisation_id = caller.key.organisation_id.clone();
    rec.event.workspace_id = caller.key.workspace_id.clone();
    rec.event.key_id = caller.key.id.clone();
    rec.set_mode("proxy");
    rec.event.interface = "proxy".into();
    rec.event.provider = provider_name.clone();
    rec.event.path = path.clone();
    rec.event.stream = true;
    // A relayed session reports nothing to count beyond itself.
    rec.usage_policy = UsagePolicy::Ignore;

    match open(&gw, &caller, &provider_name, &parts.uri, &path, &parts.headers, &mut rec).await {
        Ok(Opened::Switched { head, io, target }) => {
            rec.event.http_status = StatusCode::SWITCHING_PROTOCOLS.as_u16();
            let mut response = switching(&head.headers);
            common::stamp(response.headers_mut(), &gw, &caller.snapshot, target.as_ref(), &rec);
            rec.timeline.client_head();
            let closing = gw.sessions.closing.clone();
            gw.sessions.tracker.spawn(async move {
                match on_client.await {
                    Ok(client_io) => {
                        let (up, down, ending) =
                            pump(TokioIo::new(client_io), TokioIo::new(io), closing, IDLE, MAX_SESSION).await;
                        rec.request_bytes = up;
                        rec.response_bytes = down;
                        match ending {
                            Ending::Failed => rec.outcome(
                                pacod_gw_events::usage::status::ERROR,
                                101,
                                ending.kind(),
                                "the provider's connection failed",
                            ),
                            _ => rec.outcome(pacod_gw_events::usage::status::OK, 101, ending.kind(), ""),
                        }
                    }
                    Err(e) => rec.outcome(
                        pacod_gw_events::usage::status::CANCELLED,
                        101,
                        "client_closed",
                        &format!("the caller's connection did not switch: {e}"),
                    ),
                }
                rec.finish();
            });
            response
        }
        Ok(Opened::Refused { response, target }) => {
            // The provider's refusal goes to the caller as it is.
            let (parts, body) = response.into_parts();
            rec.event.http_status = parts.status.as_u16();
            let owner = CredentialOwner::Own;
            let mut head = Response::new(());
            *head.status_mut() = parts.status;
            *head.headers_mut() = common::client_headers(&parts.headers, owner);
            common::stamp(head.headers_mut(), &gw, &caller.snapshot, target.as_ref(), &rec);
            let bytes = http_body_util::BodyExt::collect(http_body_util::Limited::new(body, 1 << 20))
                .await
                .map(|c| c.to_bytes())
                .unwrap_or_default();
            rec.response_bytes = bytes.len() as u64;
            rec.complete_from_http();
            rec.finish();
            let (head, ()) = head.into_parts();
            Response::from_parts(head, Body::from(bytes))
        }
        Err(e) => common::fail(&e, envelope, rec),
    }
}

/// Checks the session, admits it, and asks the provider to open it.
async fn open(
    gw: &Gw,
    caller: &Caller,
    provider_name: &str,
    uri: &http::Uri,
    path: &str,
    headers: &HeaderMap,
    rec: &mut CallRecorder,
) -> Result<Opened, GwError> {
    if gw.refuses_unaccounted(caller.workspace.settings.on_spool_full) {
        return Err(GwError::overloaded(
            "the event spool is full and this workspace is not served without recording usage",
        ));
    }
    if gw.live.in_flight() > gw.boot.max_in_flight {
        return Err(GwError::overloaded("too many requests in progress on this instance"));
    }
    let snap = &caller.snapshot;
    let provider = snap
        .provider_by_name(provider_name)
        .filter(|p| p.doc.status == Status::Active)
        .cloned()
        .ok_or_else(|| GwError::not_found("provider_not_found", "no such provider"))?;
    if provider.vertex.is_some() {
        return Err(GwError::not_allowed("Vertex AI is relayed over HTTP only, not as a WebSocket"));
    }
    if !provider.doc.allows_proxy_path(path) {
        return Err(GwError::not_allowed(format!("the path {path} is not relayed for this provider")));
    }
    if caller.key.usage_policy(&caller.workspace) == UsagePolicy::Require {
        return Err(GwError::not_allowed(
            "a WebSocket session relayed byte for byte reports no usage, and this key requires it: use the managed \
             /v1/realtime, which reads it",
        ));
    }
    if caller.workspace.enforces_plugins() {
        return Err(GwError::not_allowed(
            "this workspace checks every call with a plugin, and a relayed WebSocket session cannot be checked",
        ));
    }
    if caller.workspace.settings.provider_tools.is_some() {
        return Err(GwError::not_allowed(
            "this workspace restricts the provider's tools, which a relayed WebSocket session could use unseen",
        ));
    }
    let credential = provider.credential(&caller.key.workspace_id, &caller.key.organisation_id);
    let owner = credential.as_ref().map_or(CredentialOwner::Shared, |(_, o)| *o);
    let scheme = provider.doc.proxy_auth_scheme(path);
    if credential.is_none() && scheme != AuthScheme::None {
        return Err(GwError::unavailable(
            "provider_unavailable",
            "no credential is configured for this provider in this workspace",
        ));
    }
    // A model named in the query is checked like any model call; without one the session
    // reaches whatever the credential reaches, so it needs one of the caller's own.
    let model = uri.query().and_then(|q| {
        url::form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "model").map(|(_, v)| v.into_owned())
    });
    let target = match model {
        Some(m) => {
            let t = resolve_upstream(snap, &caller.workspace, &caller.key, &provider, &m)?;
            common::record_target(snap, &t, rec);
            Some(t)
        }
        None if owner == CredentialOwner::Shared => {
            return Err(GwError::own_credential_required(
                "a WebSocket session that names no model in its query needs a credential of this workspace or \
                 organisation",
            ));
        }
        None => None,
    };
    // A relayed session is not read, so audit mode cannot record it.
    let template = target.as_ref().and_then(|t| t.template.as_deref());
    if crate::relay::audit::refuses_unrecordable(
        &caller.workspace,
        template,
        common::audit_asked(headers),
        chrono::Utc::now(),
        crate::relay::audit::Unrecordable::Session,
    ) {
        return Err(GwError::not_allowed(
            "this workspace records its sessions in full, and a WebSocket session relayed byte for byte cannot be \
             recorded: use the managed /v1/realtime",
        ));
    }
    common::admit_slot(gw, caller, rec)?;
    common::admit_limits(gw, caller, rec).map_err(|(e, _)| e)?;

    let auth = common::gw_auth_header(gw, scheme, credential.as_ref().map(|(s, _)| s.as_ref())).await?;
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    let url = format!("{}{path}{query}", provider.proxy_origin);
    let mut request =
        http::Request::builder().method(Method::GET).uri(&url).body(Body::empty()).map_err(GwError::internal)?;
    *request.headers_mut() = handshake_headers(headers, &auth);
    rec.timeline.attempt();
    rec.event.attempts = 1;
    let answer = gw.upstream.upgrade(request, HANDSHAKE).await.map_err(send_error)?;
    rec.timeline.upstream_head(Instant::now());
    Ok(match answer {
        Upgrade::Switched { head, io } => Opened::Switched { head, io, target },
        Upgrade::Refused(response) => Opened::Refused { response, target },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_websocket_handshake_is_recognised() {
        let mut h = HeaderMap::new();
        h.insert(CONNECTION, HeaderValue::from_static("keep-alive, Upgrade"));
        h.insert(UPGRADE, HeaderValue::from_static("WebSocket"));
        assert!(is_upgrade(&Method::GET, &h));
        assert!(!is_upgrade(&Method::POST, &h));
        h.insert(UPGRADE, HeaderValue::from_static("h2c"));
        assert!(!is_upgrade(&Method::GET, &h));
    }

    #[test]
    fn a_key_offered_as_a_subprotocol_is_taken_and_the_others_kept() {
        let mut h = HeaderMap::new();
        h.insert(
            SEC_WEBSOCKET_PROTOCOL,
            HeaderValue::from_static("realtime, openai-insecure-api-key.pgw_x_123, openai-beta.realtime-v1"),
        );
        take_protocol_key(&mut h);
        assert_eq!(h[SEC_WEBSOCKET_PROTOCOL], "realtime, openai-beta.realtime-v1");
        assert_eq!(h[axum::http::header::AUTHORIZATION], "Bearer pgw_x_123");
        let mut only = HeaderMap::new();
        only.insert(SEC_WEBSOCKET_PROTOCOL, HeaderValue::from_static("openai-insecure-api-key.pgw_y"));
        take_protocol_key(&mut only);
        assert!(only.get(SEC_WEBSOCKET_PROTOCOL).is_none());
    }

    #[test]
    fn the_handshake_to_the_provider_carries_its_credential_not_the_callers() {
        let mut h = HeaderMap::new();
        h.insert(axum::http::header::AUTHORIZATION, HeaderValue::from_static("Bearer pgw_caller"));
        h.insert("sec-websocket-key", HeaderValue::from_static("dGhlIHNhbXBsZSBub25jZQ=="));
        h.insert("sec-websocket-version", HeaderValue::from_static("13"));
        h.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
        let auth = common::key_header(AuthScheme::Bearer, "sk-provider").unwrap();
        let out = handshake_headers(&h, &auth);
        assert_eq!(out[axum::http::header::AUTHORIZATION], "Bearer sk-provider");
        assert_eq!((out[CONNECTION].to_str().unwrap(), out[UPGRADE].to_str().unwrap()), ("Upgrade", "websocket"));
        assert_eq!(out["sec-websocket-key"], "dGhlIHNhbXBsZSBub25jZQ==");
        assert!(out.get(axum::http::header::CONTENT_TYPE).is_none());
    }

    #[tokio::test]
    async fn bytes_are_relayed_both_ways_until_an_end_closes() {
        let (client, mut client_far) = tokio::io::duplex(1024);
        let (provider, mut provider_far) = tokio::io::duplex(1024);
        let token = tokio_util::sync::CancellationToken::new();
        let pumping = tokio::spawn(pump(client, provider, token, Duration::from_secs(5), Duration::from_secs(5)));
        client_far.write_all(b"hello").await.unwrap();
        let mut buf = [0u8; 5];
        provider_far.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"hello");
        provider_far.write_all(b"back!").await.unwrap();
        client_far.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"back!");
        drop(client_far);
        let (up, down, ending) = pumping.await.unwrap();
        assert_eq!((up, down, ending), (5, 5, Ending::Closed));
    }

    #[tokio::test]
    async fn a_quiet_session_ends_and_so_does_one_when_the_instance_stops() {
        let (client, _keep_client) = tokio::io::duplex(64);
        let (provider, _keep_provider) = tokio::io::duplex(64);
        let token = tokio_util::sync::CancellationToken::new();
        let (_, _, ending) = pump(client, provider, token, Duration::from_millis(50), Duration::from_secs(5)).await;
        assert_eq!(ending, Ending::Idle);
        let (client, _keep_client) = tokio::io::duplex(64);
        let (provider, _keep_provider) = tokio::io::duplex(64);
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        let (_, _, ending) = pump(client, provider, token, Duration::from_secs(5), Duration::from_secs(5)).await;
        assert_eq!(ending, Ending::Stopping);
    }
}
