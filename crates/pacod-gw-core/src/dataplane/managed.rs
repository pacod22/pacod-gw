//! Managed mode (concept section 4.3): one pipeline behind the OpenAI-compatible, the
//! Anthropic-compatible and the native interface.
//!
//! The caller's interface and the model's provider decide the route:
//!
//! - **Same dialect** (OpenAI interface to an OpenAI-compatible provider, Anthropic interface
//!   to Anthropic): the body is scanned and patched, so the conversation content reaches the
//!   provider as the bytes the caller sent (`openai.rs`, `anthropic.rs`).
//! - **Translated** (everything else, and always on the native interface): the request goes
//!   through the neutral model (`translate.rs`).
//!
//! Everything around that is shared: gateway parameters, resolution, admission, retries, the
//! response, and settlement.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::Response;
use bytes::{Bytes, BytesMut};
use http_body_util::BodyExt;
use pacod_gw_dialects::ClientStream;
use pacod_gw_dialects::ProviderStream;
use pacod_gw_dialects::canonical::{Event, Profile, Stamp};
use serde_json::{Value, json};

use super::Gw;
use super::auth::{Caller, authenticate};
use super::common::{self, GatewayParams, SendPlan};
use super::errors::{Envelope, GwError};
use super::{anthropic, openai, shape, translate};
use crate::cost::Costed;
use crate::extract::{Dialect, Extracted, UsageTap, parse_whole};
use crate::json::{Kind, Op, Patch, apply, scan};
use crate::model::{AuthScheme, Family, Mode, ModelKind, UsagePolicy};
use crate::relay::{CallRecorder, RelayBody, Transform};
use crate::resolve::{ResolveRequest, Target, resolve};
use crate::snapshot::CredentialOwner;
use crate::sse::SseFramer;
use crate::telemetry::metrics;
use crate::telemetry::trace::TraceContext;

/// A non-streamed answer is held so that it can be accounted before it is answered. Larger
/// answers end the call with an error.
const MAX_HELD_RESPONSE: usize = 32 << 20;

/// How much of a provider's error body is read.
const MAX_ERROR_BODY: usize = 1 << 20;

/// The interface a caller speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interface {
    OpenAi,
    Anthropic,
    Native,
}

impl Interface {
    pub fn name(self) -> &'static str {
        match self {
            Interface::OpenAi => "openai",
            Interface::Anthropic => "anthropic",
            Interface::Native => "native",
        }
    }

    pub fn envelope(self) -> Envelope {
        match self {
            Interface::OpenAi => Envelope::OpenAi,
            Interface::Anthropic => Envelope::Anthropic,
            Interface::Native => Envelope::Gateway,
        }
    }

    pub fn path(self) -> &'static str {
        match self {
            Interface::OpenAi => "/v1/chat/completions",
            Interface::Anthropic => "/v1/messages",
            Interface::Native => "/gw/v1/chat",
        }
    }
}

/// The dialect a provider is spoken to in managed mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    OpenAi,
    Anthropic,
}

impl Wire {
    /// Gemini and Ollama are reached through their OpenAI-compatible endpoints.
    pub fn of(family: Family) -> Self {
        match family {
            Family::Anthropic => Wire::Anthropic,
            Family::Openai | Family::Gemini | Family::Ollama => Wire::OpenAi,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Wire::OpenAi => "openai",
            Wire::Anthropic => "anthropic",
        }
    }

    fn dialect(self) -> Dialect {
        match self {
            Wire::OpenAi => Dialect::OpenAiChat,
            Wire::Anthropic => Dialect::Anthropic,
        }
    }
}

/// The provider request a route produced, and what it decided on the way.
#[derive(Debug)]
pub struct Shaped {
    pub body: Bytes,
    pub stream: bool,
    /// OpenAI interface: the caller asked for the usage chunk on the stream.
    pub wants_usage: bool,
    /// Parameters and features that were left out because the model does not take them.
    pub dropped: Vec<String>,
    /// Values that were changed to fit, with the reason.
    pub adjusted: Vec<String>,
}

/// The output-token limit for a provider that requires one when the request and the
/// template name none: the model's own ceiling, lowered by the grant.
pub fn default_max_output(target: &Target) -> u64 {
    let ceiling = [target.model.doc.max_output_tokens, target.terms.max_output_tokens].into_iter().flatten().min();
    u64::from(ceiling.unwrap_or(4096))
}

/// Everything that is decided about a request before the provider is called. It needs no
/// I/O, so the playground can show it without sending anything.
pub struct Plan {
    pub iface: Interface,
    pub wire: Wire,
    /// False when the request went through the neutral model.
    pub same_dialect: bool,
    pub target: Target,
    pub params: GatewayParams,
    pub model_requested: String,
    pub shaped: Shaped,
    /// Set on the translated route: what the provider half was told about the model.
    pub profile: Option<Profile>,
    /// Tag keys the workspace has not declared. They are dropped; the caller is told.
    pub warnings: Vec<String>,
}

impl Plan {
    /// The URL of the provider call.
    pub fn url(&self) -> String {
        self.target.provider.doc.managed_url(match self.wire {
            Wire::OpenAi => "chat/completions",
            Wire::Anthropic => "v1/messages",
        })
    }
}

/// Reads a request and decides how it will be served.
pub fn plan(caller: &Caller, iface: Interface, headers: &HeaderMap, bytes: &Bytes) -> Result<Plan, GwError> {
    let top =
        scan(bytes).map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?;

    let mut params = GatewayParams::from_headers(headers)?;
    if let Some(e) = top.get("gateway") {
        params.merge_object(&bytes[e.value.clone()])?;
    }
    // The dialect's own field for the end user is used when the gateway's is not given.
    if params.end_user.is_none() {
        params.end_user = match iface {
            Interface::OpenAi => top.str(bytes, "user"),
            Interface::Anthropic => shape::inner(bytes, &top, "metadata").and_then(|(raw, m)| m.str(raw, "user_id")),
            Interface::Native => None,
        }
        .filter(|u| !u.is_empty())
        .map(|u| u.chars().take(200).collect());
    }

    let model = top.str(bytes, "model").unwrap_or_default();
    if model.is_empty() && params.template.is_none() {
        return Err(GwError::invalid_request("model is required: a model id or @template"));
    }
    if top.get("messages").is_none_or(|e| e.kind != Kind::Array) {
        return Err(GwError::invalid_request("messages is required and must be a list"));
    }
    let model_requested =
        if model.is_empty() { format!("@{}", params.template.as_deref().unwrap_or_default()) } else { model.clone() };

    let snap = &caller.snapshot;
    let target = resolve(
        snap,
        &caller.workspace,
        &caller.key,
        &ResolveRequest { model: &model, template: params.template.as_deref(), provider: None, families: None },
    )?;
    if target.model.doc.kind != ModelKind::Chat {
        return Err(GwError::invalid_request(format!("the model \"{}\" is not a chat model", target.model.doc.name)));
    }

    let wire = Wire::of(target.provider.doc.family);
    let policy = params
        .unsupported_params
        .or(target.template.as_ref().and_then(|t| t.spec.unsupported_params))
        .unwrap_or(caller.workspace.settings.unsupported_params);
    let same_dialect =
        matches!((iface, wire), (Interface::OpenAi, Wire::OpenAi) | (Interface::Anthropic, Wire::Anthropic));
    let (shaped, profile) = match (same_dialect, wire) {
        (true, Wire::OpenAi) => (openai::shape_same(bytes, &top, &target, &params, policy)?, None),
        (true, Wire::Anthropic) => {
            let allowed = caller.workspace.settings.provider_tools.as_deref();
            (anthropic::shape_same(bytes, &top, &target, &params, policy, allowed)?, None)
        }
        (false, _) => {
            let (shaped, profile) = translate::shape(iface, wire, bytes, &target, &params, policy)?;
            (shaped, Some(profile))
        }
    };

    let declared = &caller.workspace.doc.tag_keys;
    let warnings = params
        .tags
        .keys()
        .filter(|k| !declared.contains(k))
        .map(|k| format!("the tag key \"{k}\" is not declared in this workspace and was dropped"))
        .chain(shaped.adjusted.iter().cloned())
        .collect();
    Ok(Plan { iface, wire, same_dialect, target, params, model_requested, shaped, profile, warnings })
}

// --- entry points --------------------------------------------------------------------------------

pub async fn chat_completions(
    State(gw): State<Arc<Gw>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Response {
    handle(gw, peer, req, Interface::OpenAi).await
}

pub async fn messages(State(gw): State<Arc<Gw>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, req: Request) -> Response {
    handle(gw, peer, req, Interface::Anthropic).await
}

pub async fn native_chat(
    State(gw): State<Arc<Gw>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Response {
    handle(gw, peer, req, Interface::Native).await
}

async fn handle(gw: Arc<Gw>, peer: SocketAddr, req: Request, iface: Interface) -> Response {
    let started = Instant::now();
    let (parts, body) = req.into_parts();
    let trace = TraceContext::from_traceparent(parts.headers.get("traceparent").and_then(|v| v.to_str().ok()));
    let caller = match authenticate(&gw, &parts.headers, Some(peer.ip()), Mode::Managed) {
        Ok(c) => c,
        Err(e) => {
            metrics::record_rejection(e.kind);
            gw.live.record_rejected();
            return e.response(iface.envelope(), "");
        }
    };
    serve(gw, caller, started, trace, parts.headers, body, iface).await
}

/// Serves a managed chat request for a caller that is already identified. The data-plane
/// entry points come here after authenticating a key; the console's playground comes here
/// with the workspace of the administrator who uses it.
pub async fn serve(
    gw: Arc<Gw>,
    caller: Caller,
    started: Instant,
    trace: TraceContext,
    headers: HeaderMap,
    body: Body,
    iface: Interface,
) -> Response {
    let mut rec = CallRecorder::new(gw.clone(), trace);
    rec.timeline = crate::relay::Timeline::starting_at(started);
    rec.event.organisation_id = caller.key.organisation_id.clone();
    rec.event.workspace_id = caller.key.workspace_id.clone();
    rec.event.key_id = caller.key.id.clone();
    rec.event.mode = "managed".into();
    rec.event.interface = iface.name().into();
    rec.event.path = iface.path().into();
    // Managed mode always asks for usage. A provider that reports none is estimated.
    rec.usage_policy = UsagePolicy::Estimate;

    match run(&gw, &caller, &headers, body, iface, &mut rec).await {
        Ok(Answer::Stream { mut head, incoming, transform, idle, target }) => {
            common::stamp(head.headers_mut(), &gw, &caller.snapshot, Some(&target), &rec);
            rec.timeline.client_head();
            let (head, ()) = head.into_parts();
            let mut body = RelayBody::new(incoming, rec, idle);
            if let Some(t) = transform {
                body = body.with_transform(t);
            }
            Response::from_parts(head, Body::new(body))
        }
        Ok(Answer::Whole { mut head, body, target, cost }) => {
            let h = head.headers_mut();
            common::stamp(h, &gw, &caller.snapshot, Some(&target), &rec);
            if let Some(c) = cost
                && let Some(amount) = c.amount
                && let Ok(v) = HeaderValue::from_str(&format!("{amount:.6} {}", c.currency))
            {
                h.insert("x-gw-cost", v);
            }
            h.insert(CONTENT_LENGTH, HeaderValue::from(body.len()));
            rec.complete_from_http();
            rec.timeline.client_head();
            rec.timeline.client_chunk(Instant::now());
            rec.finish();
            let (head, ()) = head.into_parts();
            Response::from_parts(head, Body::from(body))
        }
        Err(e) => common::fail(&e, iface.envelope(), rec),
    }
}

enum Answer {
    Stream {
        head: Response<()>,
        incoming: hyper::body::Incoming,
        /// `None` relays the provider's stream as it is.
        transform: Option<Box<dyn Transform>>,
        idle: Duration,
        target: Target,
    },
    Whole {
        head: Response<()>,
        body: Bytes,
        target: Target,
        cost: Option<Costed>,
    },
}

/// The message of a provider's error body, shortened.
fn provider_message(body: &[u8]) -> String {
    let parsed: Option<Value> = serde_json::from_slice(body).ok();
    let message = parsed
        .as_ref()
        .and_then(|v| v.pointer("/error/message").or_else(|| v.pointer("/message")).or_else(|| v.pointer("/error")))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| String::from_utf8_lossy(body).into_owned());
    message.chars().take(300).collect()
}

/// The `gateway` block of a native response (concept section 9.2).
fn gateway_block(caller: &Caller, rec: &CallRecorder, warnings: &[String]) -> Value {
    let e = &rec.event;
    json!({
        "request_id": rec.request_id(),
        "workspace": caller.workspace.doc.name,
        "template": if e.template.is_empty() { Value::Null } else { json!(e.template) },
        "template_version": if e.template.is_empty() { Value::Null } else { json!(e.template_version) },
        "model_requested": e.model_requested,
        "model_served": e.model_served,
        "provider": e.provider,
        "attempts": e.attempts,
        "latency_ms": rec.timeline.so_far(),
        "dropped_params": e.dropped_params,
        "warnings": warnings,
    })
}

/// A provider's answer as far as its head, and what is needed to read the rest.
struct Upstream {
    status: StatusCode,
    upstream_headers: HeaderMap,
    content_type: String,
    incoming: hyper::body::Incoming,
    /// The head of the answer to the caller: the provider's status and headers, filtered.
    head: Response<()>,
    /// When the whole answer must be complete, for answers that are held.
    deadline: Instant,
    timeout: Duration,
}

/// Admits the call, attaches the credential and sends the request, with the retries of
/// managed mode. Shared by chat, token counting and speech.
#[allow(clippy::too_many_arguments)]
async fn call_provider(
    gw: &Arc<Gw>,
    caller: &Caller,
    headers: &HeaderMap,
    iface: Interface,
    wire: Wire,
    target: &Target,
    params: &GatewayParams,
    url: String,
    body: &Bytes,
    stream: bool,
    rec: &mut CallRecorder,
) -> Result<Upstream, GwError> {
    let provider = &target.provider;
    let credential = provider.credential(&caller.key.workspace_id, &caller.key.organisation_id);
    let owner = credential.as_ref().map_or(CredentialOwner::Shared, |(_, o)| *o);
    let scheme = provider.doc.managed_auth_scheme();
    if credential.is_none() && scheme != AuthScheme::None {
        return Err(GwError::unavailable(
            "provider_unavailable",
            "no credential is configured for this provider in this workspace",
        ));
    }

    common::admit(gw, caller, rec)?;

    let secret = credential.as_ref().map(|(s, _)| s.value.as_str());
    let (timeout, max_attempts) =
        common::timeout_and_attempts(Some(target), provider.doc.default_timeout_seconds, params);
    // Beta features are the caller's to ask for only where the caller speaks Anthropic's dialect.
    let forward: &[&'static str] = if iface == Interface::Anthropic { &["anthropic-beta"] } else { &[] };
    let mut out_headers = common::managed_headers(headers, forward, scheme, secret, body.len(), stream)?;
    if wire == Wire::Anthropic {
        let version = headers
            .get("anthropic-version")
            .filter(|_| iface == Interface::Anthropic)
            .cloned()
            .unwrap_or(HeaderValue::from_static("2023-06-01"));
        out_headers.insert("anthropic-version", version);
    }
    let send_plan = SendPlan { method: Method::POST, url, headers: out_headers, timeout, max_attempts, managed: true };
    let sending = Instant::now();
    let sent = common::send(gw, &send_plan, Body::from(body.clone()), Some(body), rec).await?;
    rec.timeline.connected(sent.connected_at);
    rec.timeline.upstream_head(sent.head_at);

    let (parts, incoming) = sent.response.into_parts();
    rec.event.http_status = parts.status.as_u16();
    if let Some(id) = common::upstream_request_id(&parts.headers) {
        rec.event.upstream_request_id = id.chars().take(128).collect();
    }
    let content_type = parts.headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
    let mut head = Response::new(());
    *head.status_mut() = parts.status;
    *head.headers_mut() = common::client_headers(&parts.headers, owner);
    // What the client gets is not always the provider's bytes, so its length is set later.
    head.headers_mut().remove(CONTENT_LENGTH);
    Ok(Upstream {
        status: parts.status,
        upstream_headers: parts.headers,
        content_type,
        incoming,
        head,
        deadline: sending + timeout,
        timeout,
    })
}

impl Upstream {
    /// Reads the rest of the answer into memory, within the time allowed for the call.
    async fn hold(&mut self, rec: &mut CallRecorder) -> Result<Bytes, GwError> {
        rec.timeline.set_buffered();
        let cap = if self.status.is_success() { MAX_HELD_RESPONSE } else { MAX_ERROR_BODY };
        let body = http_body_util::Limited::new(&mut self.incoming, cap).collect();
        let held = tokio::time::timeout_at(self.deadline.into(), body).await;
        rec.timeline.upstream_chunk(Instant::now());
        let body = match held {
            Err(_) => {
                return Err(GwError::upstream_timeout(format!(
                    "the provider did not finish within {:?}",
                    self.timeout
                )));
            }
            Ok(Err(e)) => return Err(GwError::upstream_error(format!("the provider's answer could not be read: {e}"))),
            Ok(Ok(c)) => c.to_bytes(),
        };
        rec.response_bytes = body.len() as u64;
        Ok(body)
    }

    /// Decides what to do with an answer that is not a success. The provider's limit and
    /// the provider's failures are the gateway's to report; what the provider says about the
    /// request itself is passed on (`Ok`) when the caller speaks the provider's dialect.
    fn refused(&mut self, body: &Bytes, same_dialect: bool, wire: Wire) -> Result<(), GwError> {
        let said = provider_message(body);
        let status = self.status.as_u16();
        if status == 429 {
            let after = self
                .upstream_headers
                .get(RETRY_AFTER)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<f64>().ok());
            return Err(GwError::upstream_rate_limited(
                format!("the provider's rate limit was reached: {said}"),
                after.map(|s| s.ceil().clamp(1.0, 3600.0) as u32),
            ));
        }
        // Anthropic's "overloaded" is passed on to an Anthropic caller as it is: its SDK knows
        // what to do with it (concept section 9.4).
        let overloaded = same_dialect && wire == Wire::Anthropic && status == 529;
        if self.status.is_server_error() && !overloaded {
            return Err(GwError::upstream_error(format!("the provider answered {status}: {said}")));
        }
        if !same_dialect {
            // The provider's error body is in a dialect the caller does not speak.
            return Err(GwError::upstream_refused(self.status, format!("the provider refused the request: {said}")));
        }
        self.head.headers_mut().insert("x-gw-error-type", HeaderValue::from_static("upstream_error"));
        Ok(())
    }
}

/// Reads the request body, within the configured size.
async fn read_body(gw: &Gw, headers: &HeaderMap, body: Body, rec: &mut CallRecorder) -> Result<Bytes, GwError> {
    if gw.usage_spool.refuses_requests() {
        return Err(GwError::overloaded(
            "the event spool is full and this installation does not serve unaccounted requests",
        ));
    }
    if gw.live.in_flight() > gw.boot.max_in_flight {
        return Err(GwError::overloaded("too many requests in progress on this instance"));
    }
    if headers.get(CONTENT_ENCODING).is_some_and(|v| v != "identity") {
        return Err(GwError::invalid_request("compressed request bodies are not accepted; send the body uncompressed"));
    }
    let limit = gw.boot.max_body_bytes;
    rec.timeline.body_begin();
    let bytes = match http_body_util::Limited::new(body, limit).collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => return Err(GwError::too_large(limit)),
    };
    rec.request_bytes = bytes.len() as u64;
    rec.timeline.body_done();
    Ok(bytes)
}

async fn run(
    gw: &Arc<Gw>,
    caller: &Caller,
    headers: &HeaderMap,
    body: Body,
    iface: Interface,
    rec: &mut CallRecorder,
) -> Result<Answer, GwError> {
    let bytes = read_body(gw, headers, body, rec).await?;
    let plan = plan(caller, iface, headers, &bytes)?;
    let snap = &caller.snapshot;
    rec.event.model_requested = plan.model_requested.clone();
    plan.params.attribute(caller, rec);
    common::record_target(snap, &plan.target, rec);
    rec.event.dropped_params = plan.shaped.dropped.clone();
    rec.event.stream = plan.shaped.stream;

    let url = plan.url();
    let Plan { target, shaped, profile, warnings, wire, same_dialect, params, .. } = plan;
    let mut up =
        call_provider(gw, caller, headers, iface, wire, &target, &params, url, &shaped.body, shaped.stream, rec)
            .await?;

    // --- a stream is passed on, or shaped event by event, as it arrives ---
    if up.status.is_success() && up.content_type.starts_with("text/event-stream") {
        rec.event.stream = true;
        rec.tap = Some(UsageTap::new(wire.dialect(), &up.content_type));
        let transform: Option<Box<dyn Transform>> = match (same_dialect, wire, profile) {
            (true, Wire::OpenAi, _) => {
                Some(Box::new(openai::ChatStream { framer: SseFramer::new(), wants_usage: shaped.wants_usage }))
            }
            // Anthropic's own events for an Anthropic caller: nothing to do.
            (true, Wire::Anthropic, _) => None,
            (false, _, profile) => {
                let profile = profile.unwrap_or_default();
                up.head.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("text/event-stream"));
                Some(Box::new(TranslateStream {
                    framer: SseFramer::new(),
                    reader: translate::provider_stream(wire, &profile),
                    writer: None,
                    iface,
                    include_usage: shaped.wants_usage,
                    model: target.model.doc.name.clone(),
                    created: rec.timeline.wall_start.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
                    native: (iface == Interface::Native).then(|| NativeInfo { caller: caller.clone(), warnings }),
                    events: Vec::new(),
                }))
            }
        };
        let idle = Duration::from_secs(gw.boot.stream_idle_seconds);
        return Ok(Answer::Stream { head: up.head, incoming: up.incoming, transform, idle, target });
    }

    // --- everything else is held, so that it can be accounted before it is answered ---
    let body = up.hold(rec).await?;
    if !up.status.is_success() {
        up.refused(&body, same_dialect, wire)?;
        return Ok(Answer::Whole { head: up.head, body, target, cost: None });
    }

    if same_dialect {
        let found = parse_whole(wire.dialect(), &body);
        let usage = found.has_usage.then_some(found.usage);
        rec.absorb(found);
        let cost = usage.as_ref().and_then(|u| rec.cost_of(u));
        let body = match (wire, usage, scan(&body)) {
            // The OpenAI dialect has a place for the cost: inside `usage`.
            (Wire::OpenAi, Some(u), Ok(top)) => openai::add_cost(&body, &top, &u, cost.as_ref()).unwrap_or(body),
            // An Anthropic answer goes back as Anthropic wrote it; the cost is in a header.
            _ => body,
        };
        return Ok(Answer::Whole { head: up.head, body, target, cost });
    }

    // --- translated: read the provider's answer, write the caller's ---
    let profile = profile.unwrap_or_default();
    let response = translate::read_response(wire, &body, &profile)?;
    rec.absorb(Extracted {
        usage: response.usage.unwrap_or_default(),
        has_usage: response.usage.is_some(),
        model: None,
        finish_reason: Some(response.native_finish.clone()),
        cut: false,
    });
    let cost = response.usage.as_ref().and_then(|u| rec.cost_of(u));
    let stamp = Stamp {
        // The native interface answers under the gateway's own id; the other two keep the
        // provider's, so that the same provider answer always gives the same bytes.
        id: if iface == Interface::Native { rec.request_id() } else { response.id.clone() },
        model: target.model.doc.name.clone(),
        created: rec.timeline.wall_start.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        cost: cost.as_ref().map(openai::cost_object),
        gateway: (iface == Interface::Native).then(|| gateway_block(caller, rec, &warnings)),
    };
    let body = translate::write_response(iface, &response, &stamp);
    up.head.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(Answer::Whole { head: up.head, body, target, cost })
}

// --- token counting and speech ---------------------------------------------------------------------

/// The auxiliary endpoints of managed mode: one request, one provider call, no dialect to
/// translate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Aux {
    /// `POST /v1/messages/count_tokens`: resolved like a message, forwarded to Anthropic, free.
    CountTokens,
    /// `POST /v1/audio/speech`: OpenAI's request shape.
    Speech,
    /// `POST /gw/v1/speech`: the native request shape.
    NativeSpeech,
}

impl Aux {
    fn interface(self) -> Interface {
        match self {
            Aux::CountTokens => Interface::Anthropic,
            Aux::Speech => Interface::OpenAi,
            Aux::NativeSpeech => Interface::Native,
        }
    }

    fn path(self) -> &'static str {
        match self {
            Aux::CountTokens => "/v1/messages/count_tokens",
            Aux::Speech => "/v1/audio/speech",
            Aux::NativeSpeech => "/gw/v1/speech",
        }
    }
}

pub async fn count_tokens(
    State(gw): State<Arc<Gw>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Response {
    handle_aux(gw, peer, req, Aux::CountTokens).await
}

pub async fn speech(State(gw): State<Arc<Gw>>, ConnectInfo(peer): ConnectInfo<SocketAddr>, req: Request) -> Response {
    handle_aux(gw, peer, req, Aux::Speech).await
}

pub async fn native_speech(
    State(gw): State<Arc<Gw>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Response {
    handle_aux(gw, peer, req, Aux::NativeSpeech).await
}

async fn handle_aux(gw: Arc<Gw>, peer: SocketAddr, req: Request, aux: Aux) -> Response {
    let started = Instant::now();
    let iface = aux.interface();
    let (parts, body) = req.into_parts();
    let trace = TraceContext::from_traceparent(parts.headers.get("traceparent").and_then(|v| v.to_str().ok()));
    let caller = match authenticate(&gw, &parts.headers, Some(peer.ip()), Mode::Managed) {
        Ok(c) => c,
        Err(e) => {
            metrics::record_rejection(e.kind);
            gw.live.record_rejected();
            return e.response(iface.envelope(), "");
        }
    };
    let mut rec = CallRecorder::new(gw.clone(), trace);
    rec.timeline = crate::relay::Timeline::starting_at(started);
    rec.event.organisation_id = caller.key.organisation_id.clone();
    rec.event.workspace_id = caller.key.workspace_id.clone();
    rec.event.key_id = caller.key.id.clone();
    rec.event.mode = "managed".into();
    rec.event.interface = iface.name().into();
    rec.event.path = aux.path().into();
    // Neither endpoint reports tokens: speech is counted in characters, counting is free.
    rec.usage_policy = UsagePolicy::Ignore;

    match run_aux(&gw, &caller, &parts.headers, body, aux, &mut rec).await {
        Ok((mut head, incoming, target)) => {
            common::stamp(head.headers_mut(), &gw, &caller.snapshot, Some(&target), &rec);
            rec.timeline.client_head();
            let (head, ()) = head.into_parts();
            let idle = Duration::from_secs(gw.boot.stream_idle_seconds);
            Response::from_parts(head, Body::new(RelayBody::new(incoming, rec, idle)))
        }
        Err(e) => common::fail(&e, iface.envelope(), rec),
    }
}

async fn run_aux(
    gw: &Arc<Gw>,
    caller: &Caller,
    headers: &HeaderMap,
    body: Body,
    aux: Aux,
    rec: &mut CallRecorder,
) -> Result<(Response<()>, hyper::body::Incoming, Target), GwError> {
    let iface = aux.interface();
    let bytes = read_body(gw, headers, body, rec).await?;
    let top =
        scan(&bytes).map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?;
    let mut params = GatewayParams::from_headers(headers)?;
    if let Some(e) = top.get("gateway") {
        params.merge_object(&bytes[e.value.clone()])?;
    }
    params.attribute(caller, rec);

    let model = top.str(&bytes, "model").unwrap_or_default();
    if model.is_empty() && params.template.is_none() {
        return Err(GwError::invalid_request("model is required: a model id or @template"));
    }
    rec.event.model_requested =
        if model.is_empty() { format!("@{}", params.template.as_deref().unwrap_or_default()) } else { model.clone() };
    let snap = &caller.snapshot;
    let target = resolve(
        snap,
        &caller.workspace,
        &caller.key,
        &ResolveRequest { model: &model, template: params.template.as_deref(), provider: None, families: None },
    )?;
    common::record_target(snap, &target, rec);
    let wire = Wire::of(target.provider.doc.family);

    let mut patch = Patch::new();
    patch.set("model", &json!(target.model.doc.upstream_id));
    patch.remove("gateway");
    let url = match aux {
        Aux::CountTokens => {
            if wire != Wire::Anthropic {
                return Err(GwError::unsupported_feature(format!(
                    "token counting is not available for the model \"{}\"",
                    target.model.doc.name
                )));
            }
            // Counting costs nothing and uses no tokens: it is recorded as a request.
            rec.pricing = crate::relay::Pricing::Unknown;
            target.provider.doc.managed_url("v1/messages/count_tokens")
        }
        Aux::Speech | Aux::NativeSpeech => {
            if target.model.doc.kind != ModelKind::Speech || wire != Wire::OpenAi {
                return Err(GwError::invalid_request(format!(
                    "the model \"{}\" is not a speech model",
                    target.model.doc.name
                )));
            }
            let input = top
                .str(&bytes, "input")
                .filter(|i| !i.is_empty())
                .ok_or_else(|| GwError::invalid_request("input is required: the text to speak"))?;
            // Speech is priced by the characters of the text.
            rec.absorb(Extracted {
                usage: pacod_gw_events::Usage { characters: input.chars().count() as u64, ..Default::default() },
                has_usage: true,
                ..Default::default()
            });
            if aux == Aux::NativeSpeech
                && let Some(format) = top.get("format")
            {
                // The native name of the field; the provider knows it as `response_format`.
                patch.push("response_format", Op::Set(bytes.slice(format.value.clone())));
                patch.remove("format");
            }
            target.provider.doc.managed_url("audio/speech")
        }
    };
    let out = apply(&bytes, &top, &patch)
        .map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?
        .to_bytes();

    let mut up = call_provider(gw, caller, headers, iface, wire, &target, &params, url, &out, false, rec).await?;
    if !up.status.is_success() {
        let body = up.hold(rec).await?;
        // These endpoints answer in the provider's shape, so a provider's refusal is the
        // caller's to read, except on the native interface.
        up.refused(&body, iface != Interface::Native, wire)?;
        return Err(GwError::upstream_refused(up.status, provider_message(&body)));
    }
    Ok((up.head, up.incoming, target))
}

/// What the native stream needs to write its closing `gateway` block.
struct NativeInfo {
    caller: Caller,
    warnings: Vec<String>,
}

/// Translates a provider's stream into the caller's dialect, event by event: the provider's
/// events are read into neutral events, and those are written in the caller's shape.
struct TranslateStream {
    framer: SseFramer,
    reader: Box<dyn ProviderStream>,
    /// Created when the stream's first event names the provider's id for the response.
    writer: Option<Box<dyn ClientStream>>,
    iface: Interface,
    include_usage: bool,
    /// The model name the caller sees.
    model: String,
    created: u64,
    native: Option<NativeInfo>,
    events: Vec<Event>,
}

impl TranslateStream {
    fn drain(&mut self, rec: &mut CallRecorder) -> Bytes {
        let mut out = BytesMut::new();
        for event in std::mem::take(&mut self.events) {
            let writer = self.writer.get_or_insert_with(|| {
                let id = match (&event, self.iface) {
                    (_, Interface::Native) => rec.request_id(),
                    (Event::MessageStart { id, .. }, _) => id.clone(),
                    _ => String::new(),
                };
                let stamp = Stamp { id, model: self.model.clone(), created: self.created, cost: None, gateway: None };
                translate::client_stream(self.iface, stamp, self.include_usage)
            });
            if let Event::MessageStop { usage, .. } = &event {
                // The cost is known now, and so is everything the gateway block reports.
                let cost = usage.as_ref().and_then(|u| rec.cost_of(u)).map(|c| openai::cost_object(&c));
                let gateway = self.native.as_ref().map(|n| gateway_block(&n.caller, rec, &n.warnings));
                writer.finalize(cost, gateway);
            }
            writer.write(&event, &mut out);
        }
        out.freeze()
    }
}

impl Transform for TranslateStream {
    fn chunk(&mut self, chunk: &Bytes, rec: &mut CallRecorder) -> Bytes {
        let (reader, events) = (&mut self.reader, &mut self.events);
        self.framer.feed(chunk, |ev| reader.read(&ev.event, &ev.data, events));
        self.drain(rec)
    }

    fn end(&mut self, rec: &mut CallRecorder) -> Bytes {
        let (reader, events) = (&mut self.reader, &mut self.events);
        self.framer.finish(|ev| reader.read(&ev.event, &ev.data, events));
        self.reader.finish(&mut self.events);
        self.drain(rec)
    }

    fn error(&mut self, kind: &str, message: &str, rec: &mut CallRecorder) -> Bytes {
        let kind = if kind == "upstream_timeout" { "upstream_timeout" } else { "upstream_error" };
        self.events.push(Event::Error { kind: kind.to_owned(), message: message.to_owned() });
        self.drain(rec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_error_messages_are_found_in_the_usual_places() {
        assert_eq!(
            provider_message(br#"{"error":{"message":"context too long","type":"invalid_request_error"}}"#),
            "context too long"
        );
        assert_eq!(
            provider_message(br#"{"type":"error","error":{"type":"overloaded_error","message":"busy"}}"#),
            "busy"
        );
        assert_eq!(provider_message(br#"{"error":"overloaded"}"#), "overloaded");
        assert_eq!(provider_message(b"upstream connect error"), "upstream connect error");
        assert_eq!(provider_message(&[b'x'; 1000]).len(), 300);
    }

    #[test]
    fn the_route_follows_interface_and_provider() {
        assert_eq!(Wire::of(Family::Gemini), Wire::OpenAi);
        assert_eq!(Wire::of(Family::Ollama), Wire::OpenAi);
        assert_eq!(Wire::of(Family::Anthropic), Wire::Anthropic);
        assert_eq!((Interface::Native.path(), Interface::Native.envelope()), ("/gw/v1/chat", Envelope::Gateway));
        assert_eq!(Interface::Anthropic.envelope(), Envelope::Anthropic);
    }
}
