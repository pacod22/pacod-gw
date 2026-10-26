//! Managed OpenAI Realtime: `GET /v1/realtime?model=…`, a WebSocket session with a realtime
//! model of the repository, resolved, admitted and priced like any managed call.
//!
//! Unlike proxy mode, which relays a session's bytes unread, the gateway ends the WebSocket on
//! both sides: it answers the caller's handshake itself and opens a session of its own to the
//! provider, with the provider's credential and a key of its own. Frames pass between the two
//! unchanged, and the gateway reads the events that matter to it:
//!
//! - `response.create` and `session.update` from the caller are checked first: the
//!   workspace's limits of tokens and spend, its provider tools, and the model, which a
//!   session may not change. An event refused is answered with an `error` event and never
//!   reaches the provider.
//! - each `response.done` from the provider settles one response: one usage event, priced at
//!   the price in effect when the session opened, counted in the limits' current windows. When
//!   a limit has nothing left afterwards, the gateway closes the session (1008): with the
//!   provider's voice activity detection, responses start without the caller asking.
//!
//! The session counts as one request and holds the key's concurrency slot until it ends. A
//! caller that leaves while a response is in progress has it cancelled; what the provider
//! reports within a few seconds is settled, otherwise the response is recorded as cancelled
//! without usage. A session without any response is recorded once, with no usage. The
//! handshake's `x-gw-request-id` names the session's first event either way, and every event
//! of the session carries the same `session_id`.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{CONNECTION, SEC_WEBSOCKET_ACCEPT, SEC_WEBSOCKET_PROTOCOL, UPGRADE, USER_AGENT};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use hyper_util::rt::TokioIo;
use pacod_gw_events::Usage;
use pacod_gw_events::usage::status;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::protocol::{CloseFrame, Role, WebSocketConfig};
use tokio_tungstenite::tungstenite::{Message, handshake};

use super::Gw;
use super::auth::{Caller, authenticate};
use super::client_ip::ClientIp;
use super::common::{self, GatewayParams};
use super::errors::{Envelope, GwError};
use super::managed::{Failure, Wire, credential_for, route_input};
use super::{tools, websocket};
use crate::extract::Extracted;
use crate::json::scan;
use crate::limits::{Applicable, Subjects};
use crate::model::{Mode, ModelKind, UsagePolicy};
use crate::relay::CallRecorder;
use crate::resolve::{ResolveRequest, RouteChoice, Target, next_target, resolve_routed};
use crate::routing::live::now_ms;
use crate::telemetry::metrics;
use crate::telemetry::trace::TraceContext;
use crate::upstream::Upgrade;

/// How often the gateway pings both ends, so that idle connections are not dropped on the way.
const PING: Duration = Duration::from_secs(30);
/// How long a response cancelled because the caller left may take to report what it used.
const CANCEL_WAIT: Duration = Duration::from_secs(5);
/// The largest message either end may send: a conversation item can carry an image.
const MAX_MESSAGE: usize = 16 << 20;

/// `/v1/realtime/…`: ephemeral keys (client secrets) and SIP calls are not served. A key
/// handed out by the provider would bypass the gateway.
pub async fn not_here() -> Response {
    GwError::not_found(
        "not_found",
        "the gateway serves Realtime sessions as WebSockets at /v1/realtime only; it hands out no client secrets: \
         a browser connects with its gateway key",
    )
    .response(Envelope::OpenAi, "")
}

/// `GET /v1/realtime?model=…` asking to open a WebSocket.
pub async fn connect(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    open_session(gw, client, req).await
}

async fn open_session(gw: Arc<Gw>, client: IpAddr, mut req: Request) -> Response {
    let started = Instant::now();
    if !websocket::is_upgrade(req.method(), req.headers()) {
        return GwError::invalid_request("/v1/realtime is a WebSocket: connect with a WebSocket handshake")
            .response(Envelope::OpenAi, "");
    }
    let version_ok = req.headers().get("sec-websocket-version").is_some_and(|v| v.as_bytes() == b"13");
    let Some(client_key) = req.headers().get("sec-websocket-key").filter(|_| version_ok).cloned() else {
        return GwError::invalid_request(
            "the WebSocket handshake needs Sec-WebSocket-Key and Sec-WebSocket-Version 13",
        )
        .response(Envelope::OpenAi, "");
    };
    // Taken before the request is taken apart: the upgrade lives in its extensions.
    let on_client = hyper::upgrade::on(&mut req);
    let (mut parts, _) = req.into_parts();
    let trace = TraceContext::from_traceparent(parts.headers.get("traceparent").and_then(|v| v.to_str().ok()));
    super::auth::take_query_key(&mut parts);
    websocket::take_protocol_key(&mut parts.headers);
    let caller = match authenticate(&gw, &parts.headers, Some(client), Mode::Managed) {
        Ok(c) => c,
        Err(e) => {
            metrics::record_rejection(e.kind);
            gw.live.record_rejection(e.kind);
            gw.live.record_rejected();
            return e.response(Envelope::OpenAi, "");
        }
    };
    let mut rec = CallRecorder::new(gw.clone(), trace);
    rec.timeline = crate::relay::Timeline::starting_at(started);
    rec.event.organisation_id = caller.key.organisation_id.clone();
    rec.event.workspace_id = caller.key.workspace_id.clone();
    rec.event.key_id = caller.key.id.clone();
    rec.set_mode("managed");
    rec.event.interface = "realtime".into();
    rec.event.path = "realtime".into();
    rec.event.stream = true;
    // Each response reports its usage; nothing is estimated from the bytes.
    rec.usage_policy = UsagePolicy::Ignore;

    let offered = protocols(&parts.headers);
    match open(&gw, &caller, &parts.uri, &parts.headers, &offered, &mut rec).await {
        Ok((head, io, target)) => {
            rec.event.http_status = StatusCode::SWITCHING_PROTOCOLS.as_u16();
            let chosen = chosen_protocol(&offered, head.headers.get(SEC_WEBSOCKET_PROTOCOL));
            let mut response = switching(&client_key, chosen.as_deref());
            common::stamp(response.headers_mut(), &gw, &caller.snapshot, Some(&target), &rec);
            rec.timeline.client_head();
            let account = Account::new(gw.clone(), caller, &target, rec);
            let closing = gw.sessions.closing.clone();
            gw.sessions.tracker.spawn(async move {
                let mut account = account;
                match on_client.await {
                    Ok(client_io) => {
                        let client = WebSocketStream::from_raw_socket(
                            TokioIo::new(client_io),
                            Role::Server,
                            Some(socket_config()),
                        )
                        .await;
                        let provider =
                            WebSocketStream::from_raw_socket(TokioIo::new(io), Role::Client, Some(socket_config()))
                                .await;
                        let times = Times {
                            idle: websocket::IDLE,
                            longest: websocket::MAX_SESSION,
                            ping: PING,
                            cancel_wait: CANCEL_WAIT,
                        };
                        let ending = relay(client, provider, &mut account, times, closing).await;
                        account.end(ending);
                    }
                    Err(e) => account.failed_to_switch(&e.to_string()),
                }
            });
            response
        }
        Err(e) => common::fail(&e, Envelope::OpenAi, rec),
    }
}

fn socket_config() -> WebSocketConfig {
    WebSocketConfig::default().max_message_size(Some(MAX_MESSAGE)).max_frame_size(Some(MAX_MESSAGE))
}

/// The subprotocols the caller offered, the gateway key already taken out.
fn protocols(headers: &HeaderMap) -> Vec<String> {
    headers
        .get_all(SEC_WEBSOCKET_PROTOCOL)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|p| p.trim().to_owned())
        .filter(|p| !p.is_empty())
        .collect()
}

/// The subprotocol the gateway answers with: the provider's choice when the caller offered it,
/// else `realtime` when offered. A browser that offered subprotocols fails a handshake that
/// names one it did not offer.
fn chosen_protocol(offered: &[String], provider: Option<&HeaderValue>) -> Option<String> {
    let provider = provider.and_then(|v| v.to_str().ok()).map(str::trim);
    if let Some(p) = provider.filter(|p| offered.iter().any(|o| o == p)) {
        return Some(p.to_owned());
    }
    offered.iter().find(|o| *o == "realtime").cloned()
}

/// The caller's 101, answered by the gateway: the accept key is the caller's, and no
/// extension is agreed (the gateway reads every frame).
fn switching(client_key: &HeaderValue, protocol: Option<&str>) -> Response {
    let mut r = Response::new(Body::empty());
    *r.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let h = r.headers_mut();
    h.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
    h.insert(UPGRADE, HeaderValue::from_static("websocket"));
    if let Ok(v) = HeaderValue::from_str(&handshake::derive_accept_key(client_key.as_bytes())) {
        h.insert(SEC_WEBSOCKET_ACCEPT, v);
    }
    if let Some(v) = protocol.and_then(|p| HeaderValue::from_str(p).ok()) {
        h.insert(SEC_WEBSOCKET_PROTOCOL, v);
    }
    r
}

/// Whether a model can hold a Realtime session, and why not.
fn refuses(target: &Target) -> Option<String> {
    let model = &target.model.doc;
    if model.kind != ModelKind::Realtime {
        return Some(format!("the model \"{}\" is not a realtime model", model.name));
    }
    if Wire::of(&target.provider.doc) != Wire::OpenAi || target.provider.vertex.is_some() {
        return Some(format!("the model \"{}\" is not served over OpenAI's Realtime API", model.name));
    }
    None
}

/// Checks the session, admits it, and opens it at the first model that switches, moving on
/// to another model of a template as its failover policy allows, until a provider has switched.
async fn open(
    gw: &Arc<Gw>,
    caller: &Caller,
    uri: &http::Uri,
    headers: &HeaderMap,
    offered: &[String],
    rec: &mut CallRecorder,
) -> Result<(http::response::Parts, hyper::upgrade::Upgraded, Target), GwError> {
    if gw.refuses_unaccounted(caller.workspace.settings.on_spool_full) {
        return Err(GwError::overloaded(
            "the event spool is full and this workspace is not served without recording usage",
        ));
    }
    if gw.live.in_flight() > gw.boot.max_in_flight {
        return Err(GwError::overloaded("too many requests in progress on this instance"));
    }
    if caller.workspace.enforces_plugins() {
        return Err(GwError::not_allowed(
            "this workspace checks every call with a plugin, and a Realtime session's audio cannot be checked",
        ));
    }
    let params = GatewayParams::from_headers(headers)?;
    params.attribute(caller, rec);
    // The responses of one session belong together.
    if rec.event.session_id.is_empty() {
        rec.event.session_id = rec.request_id();
    }
    let model = uri
        .query()
        .and_then(|q| {
            url::form_urlencoded::parse(q.as_bytes()).find(|(k, _)| k == "model").map(|(_, v)| v.into_owned())
        })
        .unwrap_or_default();
    if model.is_empty() && params.template.is_none() {
        return Err(GwError::invalid_request(
            "model is required in the query: /v1/realtime?model=<model or @template>",
        ));
    }
    rec.event.model_requested =
        if model.is_empty() { format!("@{}", params.template.as_deref().unwrap_or_default()) } else { model.clone() };
    let snap = &caller.snapshot;
    let req = ResolveRequest { model: &model, template: params.template.as_deref(), provider: None, families: None };
    let (mut target, mut route) =
        resolve_routed(snap, &caller.workspace, &caller.key, &req, &route_input(&params, "realtime", true, 0, None))?;
    // Models of the pool that cannot hold a session are passed over.
    let advance = |target: &mut Target, route: &mut Option<RouteChoice>| -> bool {
        let (Some(route), Some(template)) = (route.as_mut(), target.template.clone()) else { return false };
        let req = ResolveRequest { model: "", template: Some(&template.name), provider: None, families: None };
        while let Some(next) = next_target(snap, &caller.workspace, &caller.key, &req, &template, route) {
            match refuses(&next) {
                Some(why) => route.skipped.push((next.pool_index.unwrap_or_default(), why)),
                None => {
                    *target = next;
                    return true;
                }
            }
        }
        false
    };
    if let Some(why) = refuses(&target)
        && !advance(&mut target, &mut route)
    {
        return Err(GwError::invalid_request(why));
    }
    common::record_target(snap, &target, rec);
    common::record_route(&target, route.as_ref(), rec);
    common::admit_slot(gw, caller, rec)?;

    let failover = target.template.as_ref().and_then(|t| Some(route.as_ref()?.action(t).failover));
    let retries = params.retries;
    loop {
        let may_move_on = |f: Failure, rec: &CallRecorder| {
            failover.is_some_and(|p| f.allowed_by(&p)) && retries.is_none_or(|cap| rec.event.attempts < cap)
        };
        let from = (target.model.doc.name.clone(), target.provider.doc.name.clone());
        let failure = match common::admit_limits(gw, caller, rec) {
            Err((e, model_specific)) => {
                if model_specific && may_move_on(Failure::LimitRefused, rec) && advance(&mut target, &mut route) {
                    Failure::LimitRefused
                } else {
                    return Err(e);
                }
            }
            Ok(()) => {
                let live = target.live().cloned();
                rec.in_flight = live.as_ref().map(|s| s.begin());
                let sent = Instant::now();
                let result = handshake_with(gw, caller, &target, headers, offered, rec).await;
                let failure = match &result {
                    Ok(Answer::Switched { .. }) => None,
                    Ok(Answer::Refused { status, headers, .. }) => Failure::of_answer(*status, headers),
                    Err(e) => Failure::of_error(e),
                };
                if let Some(s) = &live {
                    match failure {
                        None if result.is_ok() => s.success(sent.elapsed()),
                        Some(Failure::RateLimited(wait)) => s.failure(now_ms(), wait),
                        Some(_) => s.failure(now_ms(), None),
                        None => {}
                    }
                }
                match failure {
                    Some(f) if may_move_on(f, rec) && advance(&mut target, &mut route) => f,
                    _ => {
                        return match result? {
                            Answer::Switched { head, io } => Ok((head, io, target)),
                            Answer::Refused { status, headers, body } => Err(refusal(status, &headers, &body)),
                        };
                    }
                }
            }
        };
        rec.in_flight = None;
        metrics::record_failover(&from.1, &from.0, failure.name());
        if failure != Failure::LimitRefused {
            gw.live.record_health(&from.1, &from.0, false, 0.0);
        }
        rec.event.models_tried.push(from.0);
        rec.event.failovers = rec.event.failovers.saturating_add(1);
        rec.event.http_status = 0;
        common::record_target(snap, &target, rec);
    }
}

/// What a provider answered the gateway's handshake.
enum Answer {
    Switched { head: http::response::Parts, io: hyper::upgrade::Upgraded },
    Refused { status: StatusCode, headers: HeaderMap, body: Bytes },
}

/// A provider's refusal of the handshake, for the caller.
fn refusal(status: StatusCode, headers: &HeaderMap, body: &[u8]) -> GwError {
    let said = common::provider_message(body);
    if status == StatusCode::TOO_MANY_REQUESTS {
        let after = headers.get(axum::http::header::RETRY_AFTER).and_then(|v| v.to_str().ok());
        let after = after.and_then(|v| v.trim().parse::<f64>().ok()).map(|s| s.ceil().clamp(1.0, 3600.0) as u32);
        return GwError::upstream_rate_limited(format!("the provider's rate limit was reached: {said}"), after);
    }
    if status.is_server_error() {
        return GwError::upstream_error(format!("the provider answered {}: {said}", status.as_u16()));
    }
    GwError::upstream_refused(status, said)
}

/// Opens the provider's side: a handshake of the gateway's own, with the provider's
/// credential, a key of its own and no extensions, and the caller's subprotocols and
/// `OpenAI-Beta`.
async fn handshake_with(
    gw: &Gw,
    caller: &Caller,
    target: &Target,
    incoming: &HeaderMap,
    offered: &[String],
    rec: &mut CallRecorder,
) -> Result<Answer, GwError> {
    let (credential, scheme) = credential_for(caller, target)?;
    let auth = common::gw_auth_header(gw, scheme, credential.as_ref().map(|(s, _)| s.as_ref())).await?;
    let upstream_id = target.model.doc.upstream_id.as_str();
    let query: String = url::form_urlencoded::Serializer::new(String::new()).append_pair("model", upstream_id).finish();
    let url = target.provider.doc.managed_url(&format!("realtime?{query}"));
    let key = handshake::client::generate_key();
    let mut request =
        http::Request::builder().method(Method::GET).uri(&url).body(Body::empty()).map_err(GwError::internal)?;
    let h = request.headers_mut();
    h.insert(CONNECTION, HeaderValue::from_static("Upgrade"));
    h.insert(UPGRADE, HeaderValue::from_static("websocket"));
    h.insert("sec-websocket-version", HeaderValue::from_static("13"));
    h.insert("sec-websocket-key", HeaderValue::from_str(&key).map_err(GwError::internal)?);
    h.insert(USER_AGENT, HeaderValue::from_static(concat!("pacod-gw/", env!("CARGO_PKG_VERSION"))));
    if !offered.is_empty()
        && let Ok(v) = HeaderValue::from_str(&offered.join(", "))
    {
        h.insert(SEC_WEBSOCKET_PROTOCOL, v);
    }
    for v in incoming.get_all("openai-beta") {
        h.append("openai-beta", v.clone());
    }
    let span = rec.trace.child();
    if let Ok(v) = HeaderValue::from_str(&rec.trace.traceparent(span)) {
        h.insert("traceparent", v);
    }
    if let Some((name, value)) = auth {
        h.insert(name, value);
    }
    rec.timeline.attempt();
    rec.event.attempts = rec.event.attempts.saturating_add(1);
    let answer = gw.upstream.upgrade(request, websocket::HANDSHAKE).await.map_err(|e| match e {
        crate::upstream::SendError::Egress(m) => GwError::not_allowed(m),
        crate::upstream::SendError::Timeout(d) => {
            GwError::upstream_timeout(format!("the provider did not open the session within {d:?}"))
        }
        other => GwError::upstream_error(other.to_string()),
    })?;
    rec.timeline.upstream_head(Instant::now());
    match answer {
        Upgrade::Switched { head, io } => {
            if let Some(id) = common::upstream_request_id(&head.headers) {
                rec.event.upstream_request_id = id.chars().take(128).collect();
            }
            let accepted = head.headers.get(SEC_WEBSOCKET_ACCEPT).and_then(|v| v.to_str().ok());
            if accepted != Some(handshake::derive_accept_key(key.as_bytes()).as_str()) {
                return Err(GwError::upstream_error(
                    "the provider's WebSocket handshake did not accept the gateway's key",
                ));
            }
            Ok(Answer::Switched { head, io })
        }
        Upgrade::Refused(response) => {
            let (parts, body) = response.into_parts();
            rec.event.http_status = parts.status.as_u16();
            let body = http_body_util::BodyExt::collect(http_body_util::Limited::new(body, 1 << 20))
                .await
                .map(|c| c.to_bytes())
                .unwrap_or_default();
            Ok(Answer::Refused { status: parts.status, headers: parts.headers, body })
        }
    }
}

// --- The session ------------------------------------------------------------------------------

/// What the session's events mean for the gateway: the checks of the caller's events, and the
/// settlement of each response.
pub(crate) trait Ledger {
    /// Checks a caller's event of type `kind` before the provider sees it.
    fn check(&mut self, kind: &str, text: &str) -> Result<(), GwError>;
    /// The provider started a response.
    fn opened(&mut self);
    /// The provider's `response.done`. Returns why the session must end, when nothing is left
    /// for another response.
    fn settled(&mut self, text: &str) -> Option<GwError>;
    /// Responses that never reported their end.
    fn abandoned(&mut self, count: usize);
}

/// The session's times: tests shorten them.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Times {
    pub idle: Duration,
    pub longest: Duration,
    pub ping: Duration,
    pub cancel_wait: Duration,
}

/// Why a session ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ending {
    ClientClosed,
    ProviderClosed,
    Idle,
    TooLong,
    Stopping,
    /// A limit had nothing left after a response.
    Exhausted,
    /// The provider's connection failed.
    Failed,
}

/// The type of an event, read from its top-level members only: audio frames are large.
fn event_type(text: &str) -> Option<String> {
    scan(text.as_bytes()).ok()?.str(text.as_bytes(), "type")
}

/// An `error` event as OpenAI sends it, for an event of the caller's the gateway refused.
fn error_event(e: &GwError, refused_event: Option<&str>) -> String {
    json!({
        "type": "error",
        "event_id": format!("gw_evt_{}", uuid::Uuid::now_v7().simple()),
        "error": {
            "type": "invalid_request_error",
            "code": e.kind,
            "message": e.message,
            "param": null,
            "event_id": refused_event,
        },
    })
    .to_string()
}

fn event_id(text: &str) -> Option<String> {
    scan(text.as_bytes()).ok()?.str(text.as_bytes(), "event_id")
}

fn close_frame(code: CloseCode, reason: &str) -> Message {
    Message::Close(Some(CloseFrame { code, reason: reason.into() }))
}

/// Relays the session's frames both ways until either end closes or a limit is reached, and
/// settles each response through `ledger`.
pub(crate) async fn relay<C, P, L>(
    mut client: WebSocketStream<C>,
    mut provider: WebSocketStream<P>,
    ledger: &mut L,
    times: Times,
    closing: tokio_util::sync::CancellationToken,
) -> Ending
where
    C: AsyncRead + AsyncWrite + Unpin,
    P: AsyncRead + AsyncWrite + Unpin,
    L: Ledger,
{
    let ends = tokio::time::sleep(times.longest);
    tokio::pin!(ends);
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + times.ping, times.ping);
    let mut last = Instant::now();
    let mut open = 0usize;
    let ending = loop {
        let quiet = tokio::time::sleep_until((last + times.idle).into());
        tokio::select! {
            m = client.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    last = Instant::now();
                    if let Some(kind) = event_type(&t)
                        && let Err(e) = ledger.check(&kind, &t)
                    {
                        let refused = event_id(&t);
                        if client.send(Message::text(error_event(&e, refused.as_deref()))).await.is_err() {
                            break Ending::ClientClosed;
                        }
                        continue;
                    }
                    if provider.send(Message::Text(t)).await.is_err() {
                        break Ending::Failed;
                    }
                }
                Some(Ok(Message::Binary(b))) => {
                    last = Instant::now();
                    if provider.send(Message::Binary(b)).await.is_err() {
                        break Ending::Failed;
                    }
                }
                // Pings are answered by each side's own socket; they are not relayed, and they
                // are no traffic: the gateway's own pings would otherwise keep a quiet session.
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break Ending::ClientClosed,
            },
            m = provider.next() => match m {
                Some(Ok(Message::Text(t))) => {
                    last = Instant::now();
                    let mut exhausted = None;
                    match event_type(&t).as_deref() {
                        Some("response.created") => {
                            open += 1;
                            ledger.opened();
                        }
                        Some("response.done") => {
                            open = open.saturating_sub(1);
                            exhausted = ledger.settled(&t);
                        }
                        _ => {}
                    }
                    if client.send(Message::Text(t)).await.is_err() {
                        break Ending::ClientClosed;
                    }
                    if let Some(e) = exhausted {
                        let _ = client.send(Message::text(error_event(&e, None))).await;
                        break Ending::Exhausted;
                    }
                }
                Some(Ok(Message::Binary(b))) => {
                    last = Instant::now();
                    if client.send(Message::Binary(b)).await.is_err() {
                        break Ending::ClientClosed;
                    }
                }
                Some(Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_))) => {}
                Some(Ok(Message::Close(frame))) => {
                    let _ = client.send(Message::Close(frame)).await;
                    break Ending::ProviderClosed;
                }
                Some(Err(_)) | None => break Ending::Failed,
            },
            _ = ping.tick() => {
                if client.send(Message::Ping(Bytes::new())).await.is_err() {
                    break Ending::ClientClosed;
                }
                if provider.send(Message::Ping(Bytes::new())).await.is_err() {
                    break Ending::Failed;
                }
            }
            _ = quiet => break Ending::Idle,
            _ = &mut ends => break Ending::TooLong,
            _ = closing.cancelled() => break Ending::Stopping,
        }
    };
    let to_client = match ending {
        Ending::ClientClosed | Ending::ProviderClosed => None,
        Ending::Idle => Some(close_frame(CloseCode::Normal, "the session was idle too long")),
        Ending::TooLong => Some(close_frame(CloseCode::Normal, "the session lasted as long as it may")),
        Ending::Stopping => Some(close_frame(CloseCode::Away, "the gateway instance is stopping")),
        Ending::Exhausted => Some(close_frame(CloseCode::Policy, "a limit of the gateway is used up")),
        Ending::Failed => Some(close_frame(CloseCode::Error, "the provider's connection failed")),
    };
    if let Some(frame) = to_client {
        let _ = tokio::time::timeout(Duration::from_secs(1), client.send(frame)).await;
    }
    // A response still in progress is cancelled, and given a moment to report what it used.
    if open > 0 && !matches!(ending, Ending::ProviderClosed | Ending::Failed) {
        let cancel = json!({ "type": "response.cancel" }).to_string();
        if provider.send(Message::text(cancel)).await.is_ok() {
            let wait = tokio::time::sleep(times.cancel_wait);
            tokio::pin!(wait);
            while open > 0 {
                tokio::select! {
                    m = provider.next() => match m {
                        Some(Ok(Message::Text(t))) => {
                            if event_type(&t).as_deref() == Some("response.done") {
                                open -= 1;
                                ledger.settled(&t);
                            }
                        }
                        Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                        Some(Ok(_)) => {}
                    },
                    _ = &mut wait => break,
                }
            }
        }
    }
    if open > 0 {
        ledger.abandoned(open);
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), provider.close(None)).await;
    ending
}

// --- Accounting -------------------------------------------------------------------------------

/// A response's usage, from `response.done`. Cached tokens are cache reads; audio and image
/// tokens are counted among the uncached input, which is what the price's audio and image
/// rates apply to (cached audio is a cache read, at the cache rate).
pub(crate) fn usage_of(response: &Value) -> Option<Usage> {
    let u = response.get("usage").filter(|u| u.is_object())?;
    let n = |path: &str| u.pointer(path).and_then(Value::as_u64).unwrap_or(0);
    let input = n("/input_tokens");
    let cached = n("/input_token_details/cached_tokens").min(input);
    Some(Usage {
        input_tokens: input - cached,
        cache_read_tokens: cached,
        audio_input_tokens: n("/input_token_details/audio_tokens")
            .saturating_sub(n("/input_token_details/cached_tokens_details/audio_tokens")),
        image_input_tokens: n("/input_token_details/image_tokens")
            .saturating_sub(n("/input_token_details/cached_tokens_details/image_tokens")),
        output_tokens: n("/output_tokens"),
        audio_output_tokens: n("/output_token_details/audio_tokens"),
        ..Default::default()
    })
}

/// How a response ended, for its event: status, error type, message and finish reason.
fn outcome_of(response: &Value) -> (&'static str, String, String, String) {
    let details = &response["status_details"];
    let reason = details["reason"].as_str().unwrap_or_default().to_owned();
    match response["status"].as_str().unwrap_or_default() {
        "cancelled" => (status::CANCELLED, "cancelled".into(), reason.clone(), reason),
        "failed" => {
            let message = details["error"]["message"].as_str().unwrap_or("the response failed").to_owned();
            (status::ERROR, "upstream_error".into(), message, String::new())
        }
        "incomplete" => (status::OK, String::new(), String::new(), reason),
        _ => (status::OK, String::new(), String::new(), "stop".into()),
    }
}

/// A session's accounting: its recorder holds the slot, the request and what resolution
/// decided; each response is settled as an event of its own, made from it.
struct Account {
    gw: Arc<Gw>,
    caller: Caller,
    session: Option<CallRecorder>,
    /// The model the session was opened with, by the provider's id and by the repository's.
    models: (String, String),
    /// When each response still in progress started.
    starts: VecDeque<Instant>,
    responses: u32,
}

impl Account {
    fn new(gw: Arc<Gw>, caller: Caller, target: &Target, session: CallRecorder) -> Self {
        let models = (target.model.doc.upstream_id.clone(), target.model.doc.name.clone());
        Self { gw, caller, session: Some(session), models, starts: VecDeque::new(), responses: 0 }
    }

    /// The limits that apply now: a session can outlast a limit's window.
    fn applicable(&self) -> Vec<Applicable> {
        let Some(s) = &self.session else { return Vec::new() };
        let subjects = Subjects {
            workspace: &self.caller.workspace,
            key: &self.caller.key,
            end_user: &s.event.end_user,
            model: &s.event.model_served,
            template: &s.event.template,
            tags: &s.event.tags,
            intent: None,
            internal: false,
        };
        self.gw.limits.applicable(&subjects, chrono::Utc::now())
    }

    fn exhausted(&self) -> Option<GwError> {
        self.gw.limits.exhausted(&self.applicable()).map(|r| GwError::from_refusal(&r))
    }

    /// The provider tools an event's `session` or `response` asks for must be allowed.
    fn check_tools(&self, text: &str, member: &str) -> Result<(), GwError> {
        let Some(allowed) = self.caller.workspace.settings.provider_tools.as_deref() else { return Ok(()) };
        let body = text.as_bytes();
        let top = scan(body).map_err(|e| GwError::invalid_request(format!("the event is not acceptable JSON: {e}")))?;
        let Some(inner) = top.get(member).filter(|e| e.kind == crate::json::Kind::Object) else { return Ok(()) };
        let inner = &body[inner.value.clone()];
        let top = scan(inner).map_err(GwError::internal)?;
        tools::check(inner, &top, allowed)
    }

    /// Writes one response's event: `response` is its `response.done` object, `None` for a
    /// response that never reported its end.
    fn write(&mut self, response: Option<&Value>) {
        let applicable: Vec<Applicable> = self.applicable().into_iter().map(Applicable::counted).collect();
        let Some(s) = &self.session else { return };
        let start = self.starts.pop_front().unwrap_or_else(Instant::now);
        let trace = TraceContext { span_id: s.trace.child(), parent_span_id: Some(s.trace.span_id), ..s.trace };
        let mut r = CallRecorder::new(self.gw.clone(), trace);
        let (fresh, timestamp) = (r.event.id, r.event.timestamp);
        r.event = s.event.clone();
        // The first response is the event the handshake's `x-gw-request-id` names; the
        // session's own recorder then writes none.
        r.event.id = if self.responses == 0 { s.event.id } else { fresh };
        r.event.timestamp = timestamp;
        r.event.attempts = 1;
        r.set_mode("managed");
        r.timeline = crate::relay::Timeline::starting_at(start);
        r.pricing = s.pricing.clone();
        r.price_variant = s.price_variant.clone();
        r.holidays = s.holidays.clone();
        r.served = s.served.clone();
        r.usage_policy = UsagePolicy::Ignore;
        // The session's request was counted when it opened; each response adds its tokens
        // and spend.
        r.limits = applicable;
        match response {
            Some(resp) => {
                if let Some(usage) = usage_of(resp) {
                    r.absorb(Extracted { usage, has_usage: true, ..Default::default() });
                }
                if let Some(id) = resp["id"].as_str() {
                    r.event.upstream_request_id = id.chars().take(128).collect();
                }
                let (st, kind, message, finish) = outcome_of(resp);
                r.outcome(st, 101, &kind, &message);
                r.event.finish_reason = finish;
            }
            None => r.outcome(
                status::CANCELLED,
                101,
                "client_closed",
                "the caller left and the cancelled response did not report what it used",
            ),
        }
        r.finish();
        self.responses += 1;
    }

    /// The session ended. Its own event is written only when no response was: then it is the
    /// one record of the session, with no usage.
    fn end(&mut self, ending: Ending) {
        let Some(mut s) = self.session.take() else { return };
        tracing::debug!(request_id = %s.request_id(), responses = self.responses, ?ending, "realtime session ended");
        if self.responses > 0 {
            s.discard();
            return;
        }
        s.absorb(Extracted { usage: Usage::default(), has_usage: true, ..Default::default() });
        match ending {
            Ending::Failed => s.outcome(status::ERROR, 101, "upstream_error", "the provider's connection failed"),
            Ending::Idle => s.outcome(status::OK, 101, "session_idle", ""),
            Ending::TooLong => s.outcome(status::OK, 101, "session_too_long", ""),
            Ending::Stopping => s.outcome(status::OK, 101, "instance_stopping", ""),
            _ => s.outcome(status::OK, 101, "", ""),
        }
        s.finish();
    }

    fn failed_to_switch(&mut self, why: &str) {
        if let Some(mut s) = self.session.take() {
            s.outcome(
                status::CANCELLED,
                101,
                "client_closed",
                &format!("the caller's connection did not switch: {why}"),
            );
            s.finish();
        }
    }
}

impl Ledger for Account {
    fn check(&mut self, kind: &str, text: &str) -> Result<(), GwError> {
        match kind {
            "response.create" => {
                self.check_tools(text, "response")?;
                self.exhausted().map_or(Ok(()), Err)
            }
            "session.update" => {
                self.check_tools(text, "session")?;
                let v: Value = serde_json::from_str(text).unwrap_or_default();
                match v.pointer("/session/model").and_then(Value::as_str) {
                    Some(m) if m != self.models.0 && m != self.models.1 => Err(GwError::not_allowed(format!(
                        "the session was opened with the model \"{}\" and keeps it",
                        self.models.1
                    ))),
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    fn opened(&mut self) {
        self.starts.push_back(Instant::now());
    }

    fn settled(&mut self, text: &str) -> Option<GwError> {
        let v: Value = serde_json::from_str(text).unwrap_or_default();
        self.write(Some(&v["response"]));
        self.exhausted()
    }

    fn abandoned(&mut self, count: usize) {
        for _ in 0..count {
            self.write(None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_counts_cached_audio_as_a_cache_read_not_as_audio() {
        let r = json!({ "usage": {
            "total_tokens": 1500, "input_tokens": 1000, "output_tokens": 500,
            "input_token_details": {
                "text_tokens": 300, "audio_tokens": 700, "image_tokens": 0, "cached_tokens": 400,
                "cached_tokens_details": { "text_tokens": 100, "audio_tokens": 300, "image_tokens": 0 }
            },
            "output_token_details": { "text_tokens": 100, "audio_tokens": 400 }
        }});
        let u = usage_of(&r).unwrap();
        assert_eq!((u.input_tokens, u.cache_read_tokens, u.audio_input_tokens), (600, 400, 400));
        assert_eq!((u.output_tokens, u.audio_output_tokens), (500, 400));
        // Without details everything is text.
        let plain = usage_of(&json!({ "usage": { "input_tokens": 10, "output_tokens": 5 } })).unwrap();
        assert_eq!((plain.input_tokens, plain.audio_input_tokens, plain.output_tokens), (10, 0, 5));
        assert!(usage_of(&json!({ "status": "completed" })).is_none());
    }

    #[test]
    fn a_response_outcome_follows_its_status() {
        assert_eq!(outcome_of(&json!({ "status": "completed" })).0, status::OK);
        let cancelled = outcome_of(&json!({ "status": "cancelled", "status_details": { "reason": "turn_detected" } }));
        assert_eq!((cancelled.0, cancelled.3.as_str()), (status::CANCELLED, "turn_detected"));
        let cut = outcome_of(&json!({ "status": "incomplete", "status_details": { "reason": "max_output_tokens" } }));
        assert_eq!((cut.0, cut.3.as_str()), (status::OK, "max_output_tokens"));
        assert_eq!(outcome_of(&json!({ "status": "failed" })).0, status::ERROR);
    }

    #[test]
    fn the_answered_subprotocol_is_one_the_caller_offered() {
        let offered = vec!["realtime".to_owned(), "openai-beta.realtime-v1".to_owned()];
        let beta = HeaderValue::from_static("openai-beta.realtime-v1");
        assert_eq!(chosen_protocol(&offered, Some(&beta)).as_deref(), Some("openai-beta.realtime-v1"));
        let other = HeaderValue::from_static("something-else");
        assert_eq!(chosen_protocol(&offered, Some(&other)).as_deref(), Some("realtime"));
        assert_eq!(chosen_protocol(&[], Some(&beta)), None);
    }

    #[test]
    fn the_callers_handshake_is_accepted_with_its_own_key() {
        let r = switching(&HeaderValue::from_static("dGhlIHNhbXBsZSBub25jZQ=="), Some("realtime"));
        assert_eq!(r.status(), StatusCode::SWITCHING_PROTOCOLS);
        // RFC 6455's example.
        assert_eq!(r.headers()[SEC_WEBSOCKET_ACCEPT], "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=");
        assert_eq!(r.headers()[SEC_WEBSOCKET_PROTOCOL], "realtime");
        assert!(r.headers().get("sec-websocket-extensions").is_none());
    }

    /// A ledger that writes down what happened.
    #[derive(Default)]
    struct Book {
        refuse_responses: bool,
        exhaust_after: Option<usize>,
        opened: usize,
        settled: Vec<String>,
        abandoned: usize,
    }

    impl Ledger for Book {
        fn check(&mut self, kind: &str, _text: &str) -> Result<(), GwError> {
            match kind {
                "response.create" if self.refuse_responses => Err(GwError::quota_exceeded("used up")),
                _ => Ok(()),
            }
        }
        fn opened(&mut self) {
            self.opened += 1;
        }
        fn settled(&mut self, text: &str) -> Option<GwError> {
            let v: Value = serde_json::from_str(text).unwrap();
            self.settled.push(v["response"]["status"].as_str().unwrap_or_default().to_owned());
            (Some(self.settled.len()) == self.exhaust_after).then(|| GwError::quota_exceeded("used up"))
        }
        fn abandoned(&mut self, count: usize) {
            self.abandoned += count;
        }
    }

    type Ends = (WebSocketStream<tokio::io::DuplexStream>, WebSocketStream<tokio::io::DuplexStream>);

    /// The caller's and the provider's far ends, and the gateway's two sockets.
    async fn sockets() -> (Ends, Ends) {
        let (gw_client, client) = tokio::io::duplex(1 << 16);
        let (gw_provider, provider) = tokio::io::duplex(1 << 16);
        let far = (
            WebSocketStream::from_raw_socket(client, Role::Client, None).await,
            WebSocketStream::from_raw_socket(provider, Role::Server, None).await,
        );
        let near = (
            WebSocketStream::from_raw_socket(gw_client, Role::Server, None).await,
            WebSocketStream::from_raw_socket(gw_provider, Role::Client, None).await,
        );
        (far, near)
    }

    fn times() -> Times {
        Times {
            idle: Duration::from_secs(5),
            longest: Duration::from_secs(10),
            ping: Duration::from_secs(30),
            cancel_wait: Duration::from_millis(300),
        }
    }

    async fn text(ws: &mut WebSocketStream<tokio::io::DuplexStream>) -> Value {
        loop {
            match ws.next().await.unwrap().unwrap() {
                Message::Text(t) => return serde_json::from_str(&t).unwrap(),
                Message::Close(_) => panic!("closed"),
                _ => {}
            }
        }
    }

    fn done(status: &str) -> Message {
        Message::text(json!({ "type": "response.done", "response": { "status": status } }).to_string())
    }

    #[tokio::test]
    async fn events_pass_both_ways_and_each_response_is_settled() {
        let ((mut client, mut provider), (c, p)) = sockets().await;
        let token = tokio_util::sync::CancellationToken::new();
        let session = tokio::spawn(async move {
            let mut book = Book::default();
            let ending = relay(c, p, &mut book, times(), token).await;
            (ending, book)
        });
        client.send(Message::text(r#"{"type":"response.create"}"#)).await.unwrap();
        assert_eq!(text(&mut provider).await["type"], "response.create");
        provider.send(Message::text(r#"{"type":"response.created"}"#)).await.unwrap();
        provider.send(done("completed")).await.unwrap();
        assert_eq!(text(&mut client).await["type"], "response.created");
        assert_eq!(text(&mut client).await["type"], "response.done");
        client.close(None).await.unwrap();
        let (ending, book) = session.await.unwrap();
        assert_eq!(ending, Ending::ClientClosed);
        assert_eq!((book.opened, book.settled, book.abandoned), (1, vec!["completed".to_owned()], 0));
    }

    #[tokio::test]
    async fn a_refused_event_is_answered_with_an_error_and_never_reaches_the_provider() {
        let ((mut client, mut provider), (c, p)) = sockets().await;
        let token = tokio_util::sync::CancellationToken::new();
        let session = tokio::spawn(async move {
            let mut book = Book { refuse_responses: true, ..Default::default() };
            relay(c, p, &mut book, times(), token).await
        });
        client.send(Message::text(r#"{"type":"response.create","event_id":"e1"}"#)).await.unwrap();
        let err = text(&mut client).await;
        assert_eq!((err["type"].as_str(), err["error"]["event_id"].as_str()), (Some("error"), Some("e1")));
        assert_eq!(err["error"]["code"], "quota_exceeded");
        client.send(Message::text(r#"{"type":"session.update"}"#)).await.unwrap();
        // The next event the provider sees is the one that was not refused.
        assert_eq!(text(&mut provider).await["type"], "session.update");
        client.close(None).await.unwrap();
        assert_eq!(session.await.unwrap(), Ending::ClientClosed);
    }

    #[tokio::test]
    async fn a_session_with_nothing_left_is_closed_with_a_policy_close() {
        let ((mut client, mut provider), (c, p)) = sockets().await;
        let token = tokio_util::sync::CancellationToken::new();
        let session = tokio::spawn(async move {
            let mut book = Book { exhaust_after: Some(1), ..Default::default() };
            relay(c, p, &mut book, times(), token).await
        });
        provider.send(Message::text(r#"{"type":"response.created"}"#)).await.unwrap();
        provider.send(done("completed")).await.unwrap();
        assert_eq!(text(&mut client).await["type"], "response.created");
        assert_eq!(text(&mut client).await["type"], "response.done");
        assert_eq!(text(&mut client).await["type"], "error");
        match client.next().await.unwrap().unwrap() {
            Message::Close(Some(f)) => assert_eq!(f.code, CloseCode::Policy),
            other => panic!("expected a close, got {other:?}"),
        }
        assert_eq!(session.await.unwrap(), Ending::Exhausted);
    }

    #[tokio::test]
    async fn a_caller_that_leaves_mid_response_has_it_cancelled_and_settled() {
        let ((mut client, mut provider), (c, p)) = sockets().await;
        let token = tokio_util::sync::CancellationToken::new();
        let session = tokio::spawn(async move {
            let mut book = Book::default();
            let ending = relay(c, p, &mut book, times(), token).await;
            (ending, book)
        });
        provider.send(Message::text(r#"{"type":"response.created"}"#)).await.unwrap();
        assert_eq!(text(&mut client).await["type"], "response.created");
        drop(client);
        // The gateway asks the provider to stop, and settles what it reports.
        assert_eq!(text(&mut provider).await["type"], "response.cancel");
        provider.send(done("cancelled")).await.unwrap();
        let (ending, book) = session.await.unwrap();
        assert_eq!(ending, Ending::ClientClosed);
        assert_eq!((book.settled, book.abandoned), (vec!["cancelled".to_owned()], 0));
    }

    #[tokio::test]
    async fn a_cancelled_response_that_never_reports_is_abandoned() {
        let ((mut client, mut provider), (c, p)) = sockets().await;
        let token = tokio_util::sync::CancellationToken::new();
        let session = tokio::spawn(async move {
            let mut book = Book::default();
            let ending = relay(c, p, &mut book, times(), token).await;
            (ending, book)
        });
        provider.send(Message::text(r#"{"type":"response.created"}"#)).await.unwrap();
        assert_eq!(text(&mut client).await["type"], "response.created");
        drop(client);
        assert_eq!(text(&mut provider).await["type"], "response.cancel");
        let (_, book) = session.await.unwrap();
        assert_eq!((book.settled.len(), book.abandoned), (0, 1));
    }

    #[tokio::test]
    async fn pings_and_their_pongs_do_not_keep_a_quiet_session_open() {
        let ((mut client, mut provider), (c, p)) = sockets().await;
        // Both far ends read, so their sockets answer the gateway's pings.
        tokio::spawn(async move { while let Some(Ok(_)) = client.next().await {} });
        tokio::spawn(async move { while let Some(Ok(_)) = provider.next().await {} });
        let token = tokio_util::sync::CancellationToken::new();
        let quick = Times {
            idle: Duration::from_millis(200),
            longest: Duration::from_secs(5),
            ping: Duration::from_millis(20),
            ..times()
        };
        assert_eq!(relay(c, p, &mut Book::default(), quick, token).await, Ending::Idle);
    }

    #[tokio::test]
    async fn a_quiet_session_ends_and_so_does_one_when_the_instance_stops() {
        let ((mut client, _provider), (c, p)) = sockets().await;
        let token = tokio_util::sync::CancellationToken::new();
        let quick = Times { idle: Duration::from_millis(100), ..times() };
        let ending = relay(c, p, &mut Book::default(), quick, token).await;
        assert_eq!(ending, Ending::Idle);
        assert!(matches!(client.next().await, Some(Ok(Message::Close(Some(_))))));

        let ((mut client, _provider), (c, p)) = sockets().await;
        let token = tokio_util::sync::CancellationToken::new();
        token.cancel();
        assert_eq!(relay(c, p, &mut Book::default(), times(), token).await, Ending::Stopping);
        match client.next().await.unwrap().unwrap() {
            Message::Close(Some(f)) => assert_eq!(f.code, CloseCode::Away),
            other => panic!("expected a close, got {other:?}"),
        }
    }
}
