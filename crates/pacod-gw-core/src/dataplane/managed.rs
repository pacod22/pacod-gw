//! Managed mode (concept section 4.3): one pipeline behind the OpenAI-compatible, the
//! Anthropic-compatible, the Gemini and the native interface.
//!
//! The caller's interface and the model's provider decide the route:
//!
//! - **Same dialect** (OpenAI interface to an OpenAI-compatible provider, Anthropic interface
//!   to Anthropic, Gemini interface to Gemini's API): the body is scanned and patched, so the
//!   conversation content reaches the provider as the bytes the caller sent (`openai.rs`,
//!   `anthropic.rs`, `gemini.rs`).
//! - **Translated** (everything else, and always on the native interface): the request goes
//!   through the neutral model (`translate.rs`).
//!
//! Everything around that is shared: gateway parameters, resolution, admission, retries, the
//! response, and settlement.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::Response;
use bytes::{Bytes, BytesMut};
use http_body_util::BodyExt;
use pacod_gw_dialects::ClientStream;
use pacod_gw_dialects::ProviderStream;
use pacod_gw_dialects::assemble::events_of;
use pacod_gw_dialects::canonical::{Event, Profile, Stamp};
use pacod_gw_dialects::embeddings::GeminiEmbed;
use pacod_gw_events::Usage;
use pacod_gw_events::usage::{cache_result, usage_source};
use pacod_gw_plugin::{Keep, PlannedCall};
use serde_json::{Value, json};

use super::Gw;
use super::auth::{Caller, authenticate};
use super::client_ip::ClientIp;
use super::common::{self, GatewayParams, SendPlan};
use super::errors::{Envelope, GwError};
use super::{anthropic, openai, shape, state, tools, translate};
use crate::cost::Costed;
use crate::extract::{Dialect, Extracted, UsageTap, parse_whole};
use crate::json::{Kind, Op, Patch, TopLevel, apply, scan};
use crate::model::{
    AuthScheme, ErrorRules, FailoverPolicy, Family, ManagedApi, Mode, ModelKind, Provider, UsagePolicy,
};
use crate::plugins::ActiveBinding;
use crate::plugins::answers::{self, Found, Kept};
use crate::relay::{CallRecorder, RelayBody, Transform};
use crate::resolve::{ResolveRequest, RouteChoice, Target, next_target, resolve_routed};
use crate::routing::RouteInput;
use crate::routing::live::now_ms;
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
    /// Gemini's API, as its SDKs call it (both their Gemini API and their Vertex AI mode).
    Gemini,
    Native,
    /// OpenAI's Responses API, stateless (`responses.rs`).
    Responses,
}

impl Interface {
    pub fn name(self) -> &'static str {
        match self {
            Interface::OpenAi => "openai",
            Interface::Anthropic => "anthropic",
            Interface::Gemini => "gemini",
            Interface::Native => "native",
            Interface::Responses => "responses",
        }
    }

    pub fn envelope(self) -> Envelope {
        match self {
            Interface::OpenAi | Interface::Responses => Envelope::OpenAi,
            Interface::Anthropic => Envelope::Anthropic,
            Interface::Gemini => Envelope::Google,
            Interface::Native => Envelope::Gateway,
        }
    }

    /// The path the request log and the audit record name. Gemini's paths name the model, so
    /// they are recorded as one template.
    pub fn path(self) -> &'static str {
        match self {
            Interface::OpenAi => "/v1/chat/completions",
            Interface::Anthropic => "/v1/messages",
            Interface::Gemini => "/v1beta/models/{model}:generateContent",
            Interface::Native => "/gw/v1/chat",
            Interface::Responses => "/v1/responses",
        }
    }
}

/// What the path of a Gemini-interface request says: Gemini's API names the model and the
/// method there, not in the body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathCall {
    /// The model or `@template`, as the caller named it.
    pub model: String,
    /// `streamGenerateContent`.
    pub stream: bool,
    /// The stream is server-sent events (`alt=sse`, what the SDKs ask for), otherwise the
    /// elements of one JSON array.
    pub sse: bool,
}

/// The dialect a provider is spoken to in managed mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    OpenAi,
    Anthropic,
    /// Gemini's own API: patched for a Gemini caller, translated for the others (and for
    /// every caller on Vertex AI).
    Gemini,
}

impl Wire {
    /// Ollama, and Gemini unless its provider says `native`, are reached through their
    /// OpenAI-compatible endpoints.
    pub fn of(provider: &Provider) -> Self {
        match (provider.family, provider.managed_api) {
            (Family::Anthropic, _) => Wire::Anthropic,
            (Family::Gemini, ManagedApi::Native) => Wire::Gemini,
            // Decision and rerank providers serve no chat: their models are of other kinds,
            // which `plan` refuses, and the endpoints of those kinds build their own calls.
            (Family::Openai | Family::Gemini | Family::Ollama | Family::Typesafe | Family::Cohere, _) => Wire::OpenAi,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Wire::OpenAi => "openai",
            Wire::Anthropic => "anthropic",
            Wire::Gemini => "gemini",
        }
    }

    fn dialect(self) -> Dialect {
        match self {
            Wire::OpenAi => Dialect::OpenAiChat,
            Wire::Anthropic => Dialect::Anthropic,
            Wire::Gemini => Dialect::Gemini,
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
    /// Where each parameter's value came from, for the audit record.
    pub sources: std::collections::BTreeMap<String, &'static str>,
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
    /// For a template: the models routing chose, and how far the call has got through them.
    pub route: Option<RouteChoice>,
    /// The request's layout, to shape it again for another model.
    top: TopLevel,
    /// What the path said, on the Gemini interface.
    call: Option<PathCall>,
    /// Responses interface: the request members its answer repeats.
    echo: Option<Value>,
}

/// How the request reaches one model.
struct Shape {
    wire: Wire,
    same_dialect: bool,
    shaped: Shaped,
    profile: Option<Profile>,
}

/// Shapes the request for a target: patched in its own dialect, or translated.
fn shape_for(
    caller: &Caller,
    iface: Interface,
    bytes: &Bytes,
    top: &TopLevel,
    target: &Target,
    params: &GatewayParams,
    call: Option<&PathCall>,
) -> Result<Shape, GwError> {
    let wire = Wire::of(&target.provider.doc);
    let policy = params
        .unsupported_params
        .or(target.template.as_ref().and_then(|t| t.spec.unsupported_params))
        .unwrap_or(caller.workspace.settings.unsupported_params);
    let same_dialect = match (iface, wire) {
        (Interface::OpenAi, Wire::OpenAi) | (Interface::Anthropic, Wire::Anthropic) => true,
        (Interface::Responses, Wire::OpenAi) => target.provider.doc.quirks.responses_api,
        (Interface::Gemini, Wire::Gemini) => super::gemini::same_dialect(bytes, target),
        _ => false,
    };
    let (shaped, profile) = match (same_dialect, wire) {
        (true, Wire::OpenAi) if iface == Interface::Responses => {
            (super::responses::shape_same(bytes, top, target, params, policy)?, None)
        }
        (true, Wire::OpenAi) => (openai::shape_same(bytes, top, target, params, policy)?, None),
        (true, Wire::Anthropic) => (anthropic::shape_same(bytes, top, target, params, policy)?, None),
        (true, Wire::Gemini) => (super::gemini::shape_same(bytes, top, target, policy, call)?, None),
        (false, _) => {
            let (shaped, profile) = translate::shape(iface, wire, bytes, target, params, policy, call)?;
            (shaped, Some(profile))
        }
    };
    // Provider tools are checked on what the provider receives, whichever way they were asked for.
    if let Some(allowed) = caller.workspace.settings.provider_tools.as_deref() {
        let top = scan(&shaped.body).map_err(GwError::internal)?;
        tools::check(&shaped.body, &top, allowed)?;
    }
    Ok(Shape { wire, same_dialect, shaped, profile })
}

/// Warnings for the caller: undeclared tag keys, and values the route had to change.
fn warnings_for(caller: &Caller, params: &GatewayParams, shaped: &Shaped) -> Vec<String> {
    let declared = &caller.workspace.settings.tag_keys;
    params
        .tags
        .keys()
        .filter(|k| !declared.contains(k))
        .map(|k| format!("the tag key \"{k}\" is not declared in this workspace and was dropped"))
        .chain(shaped.adjusted.iter().cloned())
        .collect()
}

static NO_FLAGS: std::collections::BTreeMap<String, f32> = std::collections::BTreeMap::new();

/// What routing looks at, from the gateway parameters, the request and what enforcing plugins
/// found (`docs/plugin-concept.md`, section 5.8).
pub fn route_input<'a>(
    params: &'a GatewayParams,
    interface: &'a str,
    stream: bool,
    body_len: usize,
    signals: Option<&'a pacod_gw_plugin::Signals>,
) -> RouteInput<'a> {
    RouteInput {
        intent: signals.and_then(|s| s.act()),
        intent_confidence: signals.and_then(|s| s.intent.as_ref()).map_or(0.0, |i| i.confidence),
        flags: signals.map_or(&NO_FLAGS, |s| &s.flags),
        complexity: params.complexity,
        agent_id: params.agent_id.as_deref(),
        task: params.task.as_deref(),
        user_id: params.user_id.as_deref(),
        end_user: params.end_user.as_deref(),
        session_id: params.session_id.as_deref(),
        template: "",
        tags: &params.tags,
        interface,
        stream,
        // About four bytes of JSON per token.
        input_tokens: u32::try_from(body_len / 4).unwrap_or(u32::MAX),
    }
}

impl Plan {
    /// The failover policy of the rule that chose the model. Unset without a template.
    pub fn failover(&self) -> Option<FailoverPolicy> {
        let t = self.target.template.as_ref()?;
        Some(self.route.as_ref()?.action(t).failover)
    }

    /// Moves on to the next model routing chose that can serve the call. Models that cannot
    /// (not a chat model, a request they cannot take) are passed over.
    fn advance(&mut self, caller: &Caller, iface: Interface, bytes: &Bytes) -> bool {
        let (Some(route), Some(template)) = (self.route.as_mut(), self.target.template.clone()) else {
            return false;
        };
        let req = ResolveRequest { model: "", template: Some(&template.name), provider: None, families: None };
        while let Some(target) = next_target(&caller.snapshot, &caller.workspace, &caller.key, &req, &template, route) {
            let at = target.pool_index.unwrap_or_default();
            if target.model.doc.kind != ModelKind::Chat {
                route.skipped.push((at, "not_a_chat_model".into()));
                continue;
            }
            match shape_for(caller, iface, bytes, &self.top, &target, &self.params, self.call.as_ref()) {
                Ok(shape) => {
                    self.warnings = warnings_for(caller, &self.params, &shape.shaped);
                    self.target = target;
                    self.wire = shape.wire;
                    self.same_dialect = shape.same_dialect;
                    self.shaped = shape.shaped;
                    self.profile = shape.profile;
                    return true;
                }
                Err(e) => route.skipped.push((at, e.kind.to_owned())),
            }
        }
        false
    }

    /// The dialect the provider answers in: its wire's, or the Responses API's.
    pub fn reads(&self) -> Dialect {
        reads(self.iface, self.wire, self.same_dialect)
    }

    /// The URL of the provider call.
    pub fn url(&self) -> String {
        let doc = &self.target.provider.doc;
        match self.wire {
            Wire::OpenAi if self.iface == Interface::Responses && self.same_dialect => doc.managed_url("responses"),
            Wire::OpenAi => doc.managed_url("chat/completions"),
            Wire::Anthropic => doc.managed_url("v1/messages"),
            // The model is part of Gemini's path; a stream is asked for as server-sent events.
            Wire::Gemini => {
                let model = self.target.model.doc.upstream_id.trim_start_matches("models/");
                if self.shaped.stream {
                    doc.managed_url(&format!("models/{model}:streamGenerateContent?alt=sse"))
                } else {
                    doc.managed_url(&format!("models/{model}:generateContent"))
                }
            }
        }
    }
}

/// The dialect a provider answers in: its wire's, except for a Responses caller relayed to a
/// provider of the Responses API.
fn reads(iface: Interface, wire: Wire, same_dialect: bool) -> Dialect {
    if iface == Interface::Responses && same_dialect { Dialect::OpenAiResponses } else { wire.dialect() }
}

/// Reads a request and decides how it will be served. `call` is what the path of a
/// Gemini-interface request said.
pub fn plan(
    caller: &Caller,
    iface: Interface,
    headers: &HeaderMap,
    bytes: &Bytes,
    call: Option<&PathCall>,
    signals: Option<&pacod_gw_plugin::Signals>,
) -> Result<Plan, GwError> {
    let top =
        scan(bytes).map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?;
    let params = read_params(iface, headers, bytes, &top)?;

    let model = match call {
        Some(c) => c.model.clone(),
        None => top.str(bytes, "model").unwrap_or_default(),
    };
    if model.is_empty() && params.template.is_none() {
        return Err(GwError::invalid_request("model is required: a model id or @template"));
    }
    if iface == Interface::Responses {
        if top.get("input").is_none_or(|e| !matches!(e.kind, Kind::Array | Kind::String)) {
            return Err(GwError::invalid_request("input is required: a string or a list of items"));
        }
        // Whichever provider serves the call, nothing is kept (see `responses.rs`).
        super::responses::refuse_stateful(bytes)?;
    } else {
        let (list, list_name) =
            if iface == Interface::Gemini { ("contents", "contents") } else { ("messages", "messages") };
        if top.get(list).is_none_or(|e| e.kind != Kind::Array) {
            return Err(GwError::invalid_request(format!("{list_name} is required and must be a list")));
        }
    }
    let model_requested =
        if model.is_empty() { format!("@{}", params.template.as_deref().unwrap_or_default()) } else { model.clone() };

    let snap = &caller.snapshot;
    let stream = match call {
        Some(c) => c.stream,
        None => top.get("stream").is_some_and(|e| &bytes[e.value.clone()] == b"true"),
    };
    let input = route_input(&params, iface.name(), stream, bytes.len(), signals);
    let (target, route) = resolve_routed(
        snap,
        &caller.workspace,
        &caller.key,
        &ResolveRequest { model: &model, template: params.template.as_deref(), provider: None, families: None },
        &input,
    )?;
    if target.model.doc.kind != ModelKind::Chat {
        return Err(GwError::invalid_request(format!("the model \"{}\" is not a chat model", target.model.doc.name)));
    }

    let Shape { wire, same_dialect, shaped, profile } = shape_for(caller, iface, bytes, &top, &target, &params, call)?;
    let warnings = warnings_for(caller, &params, &shaped);
    Ok(Plan {
        iface,
        wire,
        same_dialect,
        target,
        params,
        model_requested,
        shaped,
        profile,
        warnings,
        route,
        top,
        call: call.cloned(),
        echo: (iface == Interface::Responses).then(|| pacod_gw_dialects::responses::echo_of(bytes)).flatten(),
    })
}

/// The gateway parameters of a managed request: the `x-gw-*` headers, the body's `gateway`
/// object, and the dialect's own field for the end user when the gateway's is not given.
fn read_params(iface: Interface, headers: &HeaderMap, bytes: &Bytes, top: &TopLevel) -> Result<GatewayParams, GwError> {
    let mut params = GatewayParams::from_headers(headers)?;
    if let Some(e) = top.get("gateway") {
        params.merge_object(&bytes[e.value.clone()])?;
    }
    if params.end_user.is_none() {
        params.end_user = match iface {
            Interface::OpenAi => top.str(bytes, "user"),
            Interface::Responses => top.str(bytes, "safety_identifier").or_else(|| top.str(bytes, "user")),
            Interface::Anthropic => shape::inner(bytes, top, "metadata").and_then(|(raw, m)| m.str(raw, "user_id")),
            Interface::Gemini | Interface::Native => None,
        }
        .filter(|u| !u.is_empty())
        .map(|u| u.chars().take(200).collect());
    }
    Ok(params)
}

/// What the workspace's plugins are shown of a managed request. `None` for a request planning
/// will refuse anyway: no plugin is asked about it.
fn plugin_input(
    caller: &Caller,
    iface: Interface,
    headers: &HeaderMap,
    bytes: &Bytes,
    call: Option<&PathCall>,
) -> Option<pacod_gw_plugin::RequestInput> {
    let top = scan(bytes).ok()?;
    let params = read_params(iface, headers, bytes, &top).ok()?;
    let model = match call {
        Some(c) => c.model.clone(),
        None => top.str(bytes, "model").unwrap_or_default(),
    };
    let model = if model.is_empty() { format!("@{}", params.template.as_deref()?) } else { model };
    let stream = match call {
        Some(c) => c.stream,
        None => top.get("stream").is_some_and(|e| &bytes[e.value.clone()] == b"true"),
    };
    let dialect = match iface {
        Interface::OpenAi => pacod_gw_plugin::Dialect::OpenAiChat,
        Interface::Anthropic => pacod_gw_plugin::Dialect::Anthropic,
        Interface::Gemini => pacod_gw_plugin::Dialect::Gemini,
        Interface::Native => pacod_gw_plugin::Dialect::Native,
        Interface::Responses => pacod_gw_plugin::Dialect::OpenAiResponses,
    };
    Some(crate::plugins::input(caller, &params, iface.name(), None, iface.path(), model, stream, bytes, dialect))
}

// --- entry points --------------------------------------------------------------------------------

pub async fn chat_completions(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    handle(gw, client, req, Interface::OpenAi).await
}

pub async fn messages(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    handle(gw, client, req, Interface::Anthropic).await
}

pub async fn native_chat(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    handle(gw, client, req, Interface::Native).await
}

pub async fn responses(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    handle(gw, client, req, Interface::Responses).await
}

async fn handle(gw: Arc<Gw>, client: IpAddr, req: Request, iface: Interface) -> Response {
    handle_call(gw, client, req, iface, None).await
}

/// A chat request; on the Gemini interface with what its path said.
pub(super) async fn handle_call(
    gw: Arc<Gw>,
    client: IpAddr,
    req: Request,
    iface: Interface,
    call: Option<PathCall>,
) -> Response {
    let started = Instant::now();
    let (parts, body) = req.into_parts();
    let trace = TraceContext::from_traceparent(parts.headers.get("traceparent").and_then(|v| v.to_str().ok()));
    let caller = match authenticate(&gw, &parts.headers, Some(client), Mode::Managed) {
        Ok(c) => c,
        Err(e) => {
            metrics::record_rejection(e.kind);
            gw.live.record_rejection(e.kind);
            gw.live.record_rejected();
            return e.response(iface.envelope(), "");
        }
    };
    serve(gw, caller, started, trace, parts.headers, body, iface, call).await
}

/// Serves a managed chat request for a caller that is already identified. The data-plane
/// entry points come here after authenticating a key; the console's playground comes here
/// with the workspace of the administrator who uses it.
#[allow(clippy::too_many_arguments)]
pub async fn serve(
    gw: Arc<Gw>,
    caller: Caller,
    started: Instant,
    trace: TraceContext,
    headers: HeaderMap,
    body: Body,
    iface: Interface,
    call: Option<PathCall>,
) -> Response {
    let mut rec = CallRecorder::new(gw.clone(), trace);
    rec.timeline = crate::relay::Timeline::starting_at(started);
    rec.event.organisation_id = caller.key.organisation_id.clone();
    rec.event.workspace_id = caller.key.workspace_id.clone();
    rec.event.key_id = caller.key.id.clone();
    if let Some(crate::dataplane::auth::Internal::Plugin { parent, .. }) = &caller.internal {
        rec.event.parent_id = parent.clone();
    }
    rec.set_mode("managed");
    rec.event.interface = iface.name().into();
    rec.event.path = iface.path().into();
    // Managed mode always asks for usage. A provider that reports none is estimated.
    rec.usage_policy = UsagePolicy::Estimate;

    match run(&gw, &caller, &headers, body, iface, call.as_ref(), &mut rec).await {
        Ok(Answer::Stream { mut head, incoming, transform, idle, target }) => {
            common::stamp(head.headers_mut(), &gw, &caller.snapshot, Some(&target), &rec);
            if let Some(a) = rec.audit.as_mut() {
                a.client_head(head.status().as_u16(), head.headers(), true);
            }
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
            if let Some(a) = rec.audit.as_mut() {
                a.client_head(head.status().as_u16(), head.headers(), false);
            }
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

use super::common::provider_message;

/// Whether an error body is in OpenAI's shape: `{"error": {"message": …}}`.
fn is_openai_error(body: &[u8]) -> bool {
    serde_json::from_slice::<Value>(body).is_ok_and(|v| v.pointer("/error/message").is_some_and(Value::is_string))
}

/// What routing decided, for the audit record.
pub fn route_resolution(target: &Target, route: Option<&RouteChoice>) -> Value {
    let (Some(t), Some(route)) = (target.template.as_ref(), route) else { return Value::Null };
    let action = route.action(t);
    let name = |i: usize| t.pool.get(i).map(|m| m.model.doc.name.clone());
    json!({
        "scheme": t.route.scheme.as_ref().map(|s| json!({ "name": &*s.name, "version": s.version })),
        "source": t.route.source.name(),
        "rule": action.rule.as_deref(),
        "algorithm": action.algorithm.name(),
        "candidates": route.candidates.iter().filter_map(name).collect::<Vec<_>>(),
        "skipped": route.skipped.iter().map(|(i, why)| json!({ "model": name(*i), "reason": why })).collect::<Vec<_>>(),
    })
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
        "routing": if e.routing_source.is_empty() { Value::Null } else { json!({
            "scheme": if e.routing_scheme.is_empty() { Value::Null } else { json!(e.routing_scheme) },
            "rule": if e.routing_rule.is_empty() { Value::Null } else { json!(e.routing_rule) },
            "source": e.routing_source,
        }) },
        "models_tried": e.models_tried,
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
    /// From sending to the response head.
    first_byte: Duration,
}

/// A credential and whose it is.
type Credential = Option<(Arc<crate::snapshot::Secret>, CredentialOwner)>;

/// The credential for a target, or why there is none.
pub(super) fn credential_for(caller: &Caller, target: &Target) -> Result<(Credential, AuthScheme), GwError> {
    let provider = &target.provider;
    let credential = provider.credential(&caller.key.workspace_id, &caller.key.organisation_id);
    let scheme = provider.doc.managed_auth_scheme();
    if credential.is_none() && scheme != AuthScheme::None {
        return Err(GwError::unavailable(
            "provider_unavailable",
            "no credential is configured for this provider in this workspace",
        ));
    }
    Ok((credential, scheme))
}

/// Attaches the credential and sends the request to one model, with the retries of managed
/// mode.
#[allow(clippy::too_many_arguments)]
async fn send_to(
    gw: &Arc<Gw>,
    caller: &Caller,
    headers: &HeaderMap,
    iface: Interface,
    wire: Wire,
    target: &Target,
    url: String,
    body: &Bytes,
    stream: bool,
    timeout: Duration,
    max_attempts: u8,
    rec: &mut CallRecorder,
    content_type: Option<&HeaderValue>,
) -> Result<Upstream, GwError> {
    let (credential, scheme) = credential_for(caller, target)?;
    let owner = credential.as_ref().map_or(CredentialOwner::Shared, |(_, o)| *o);
    let secret = credential.as_ref().map(|(s, _)| s.as_ref());
    // Files the provider is sent must be this workspace's (concept section 8.3), whichever
    // interface the call came in on. A form (an upload) names no file.
    if matches!(wire, Wire::Anthropic | Wire::OpenAi) && content_type.is_none() {
        super::files::check(gw, caller, owner, body).await?;
    }
    if wire == Wire::OpenAi && content_type.is_none() {
        super::files::check_openai(owner, body)?;
    }
    // Gemini's files and context caches only on a credential of the caller's own.
    if wire == Wire::Gemini {
        super::files::check_gemini(owner, body, target.provider.vertex.is_some())?;
    }
    // Beta features reach an Anthropic model whichever interface asked for them, within what
    // the installation allows.
    let forward: &[&'static str] = if wire == Wire::Anthropic { &["anthropic-beta"] } else { &[] };
    if wire == Wire::Anthropic {
        common::check_anthropic_betas(headers, &caller.snapshot)?;
    }
    let auth = common::gw_auth_header(gw, scheme, secret).await?;
    let mut out_headers = common::managed_headers(headers, forward, &auth, body.len(), stream, content_type);
    if wire == Wire::Anthropic {
        let version = headers
            .get("anthropic-version")
            .filter(|_| iface == Interface::Anthropic)
            .cloned()
            .unwrap_or(HeaderValue::from_static("2023-06-01"));
        out_headers.insert("anthropic-version", version);
    }
    let mut send_plan = SendPlan {
        method: Method::POST,
        url,
        headers: out_headers,
        timeout,
        max_attempts,
        managed: true,
        error_rules: Some(&target.provider.doc.quirks.error_rules),
    };
    let sending = Instant::now();
    let mut sent = common::send(gw, &send_plan, Body::from(body.clone()), Some(body), rec).await?;
    // A service account's token the provider refused (revoked early, or the clock was off):
    // forgotten, minted again, and the call sent once more.
    if sent.response.status() == StatusCode::UNAUTHORIZED
        && scheme == crate::model::AuthScheme::GoogleServiceAccount
        && let (Some(secret), Some(token)) = (secret, common::bearer_token(&auth))
    {
        gw.google.invalidate(&secret.id, token);
        if let Some((name, value)) = common::gw_auth_header(gw, scheme, Some(secret)).await? {
            send_plan.headers.insert(name, value);
        }
        sent = common::send(gw, &send_plan, Body::from(body.clone()), Some(body), rec).await?;
    }
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
        first_byte: sent.head_at.saturating_duration_since(sending),
    })
}

/// Why a model did not serve a call, as far as moving on to another model is concerned.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) enum Failure {
    /// `429`, with the wait the provider asked for.
    RateLimited(Option<Duration>),
    ServerError,
    /// No connection, a broken one, or no credential for the provider.
    Connect,
    Timeout,
    /// A limit of the gateway that counts only this model.
    LimitRefused,
}

impl Failure {
    fn of(result: &Result<Upstream, GwError>) -> Option<Self> {
        match result {
            Ok(up) => Self::of_answer(up.status, &up.upstream_headers),
            Err(e) => Self::of_error(e),
        }
    }

    /// A provider's answer that a call may move on from: its limit, or its failure.
    pub(super) fn of_answer(status: StatusCode, headers: &HeaderMap) -> Option<Self> {
        // A used-up balance (DeepSeek's 402) is this provider's limit too.
        if status == StatusCode::PAYMENT_REQUIRED {
            return Some(Self::RateLimited(None));
        }
        if status == StatusCode::TOO_MANY_REQUESTS {
            return Some(Self::RateLimited(
                headers
                    .get(RETRY_AFTER)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.trim().parse::<f64>().ok())
                    .filter(|s| s.is_finite() && *s >= 0.0)
                    .map(Duration::from_secs_f64),
            ));
        }
        // Anthropic's 529 "overloaded" is among them.
        status.is_server_error().then_some(Self::ServerError)
    }

    /// A call that got no answer: a timeout, or no connection.
    pub(super) fn of_error(e: &GwError) -> Option<Self> {
        match e.kind {
            "upstream_timeout" => Some(Self::Timeout),
            "upstream_error" | "provider_unavailable" => Some(Self::Connect),
            // The credential's quota at this provider is used up; another model may serve.
            "upstream_quota_exhausted" => Some(Self::RateLimited(None)),
            _ => None,
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::RateLimited(_) => "rate_limited",
            Self::ServerError => "server_error",
            Self::Connect => "connect_error",
            Self::Timeout => "timeout",
            Self::LimitRefused => "limit_refused",
        }
    }

    pub(super) fn allowed_by(self, p: &FailoverPolicy) -> bool {
        match self {
            Self::RateLimited(_) => p.on.rate_limited,
            Self::ServerError => p.on.server_error,
            Self::Connect => p.on.connect_error,
            Self::Timeout => p.on.timeout,
            Self::LimitRefused => p.on.limit_refused,
        }
    }
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
        if let Some(a) = rec.audit.as_mut() {
            a.upstream_chunk(&body);
        }
        Ok(body)
    }

    /// Decides what to do with an answer that is not a success. The provider's limit and
    /// the provider's failures are the gateway's to report; what the provider says about the
    /// request itself is passed on (`Ok`) when the caller speaks the provider's dialect.
    fn refused(&mut self, body: &Bytes, same_dialect: bool, wire: Wire, rules: &ErrorRules) -> Result<(), GwError> {
        let said = provider_message(body);
        let status = self.status.as_u16();
        if status == 429 && rules.is_quota(body) {
            return Err(GwError::upstream_quota_exhausted(format!(
                "the provider says the credential's quota is used up: {said}"
            )));
        }
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
        // A body in another shape than the caller's SDK reads (Gemini's compatible endpoint
        // wraps its errors in an array, Ollama's native errors are a string) is answered in
        // the gateway's own envelope of the caller's dialect, with the provider's status.
        let readable = !same_dialect || wire != Wire::OpenAi || is_openai_error(body);
        if !same_dialect || !readable {
            // The provider's error body is in a dialect the caller does not speak.
            return Err(GwError::upstream_refused(self.status, format!("the provider refused the request: {said}")));
        }
        self.head.headers_mut().insert("x-gw-error-type", HeaderValue::from_static("upstream_error"));
        Ok(())
    }
}

/// Reads the request body, within the configured size.
async fn read_body(
    gw: &Gw,
    caller: &Caller,
    headers: &HeaderMap,
    body: Body,
    rec: &mut CallRecorder,
) -> Result<Bytes, GwError> {
    if gw.refuses_unaccounted(caller.workspace.settings.on_spool_full) {
        return Err(GwError::overloaded(
            "the event spool is full and this workspace is not served without recording usage",
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
    call: Option<&PathCall>,
    rec: &mut CallRecorder,
) -> Result<Answer, GwError> {
    let bytes = read_body(gw, caller, headers, body, rec).await?;
    // A Responses call that continues a stored response is served from here on as the
    // stateless call its caller could have sent (`state.rs`); the audit record receives the
    // caller's own body.
    let received = bytes.clone();
    let (bytes, mut turn) = match iface {
        Interface::Responses => super::state::prepare(gw, caller, bytes, rec.event.id).await?,
        _ => (bytes, None),
    };
    // Once per call, before planning: what enforcing plugins find can route the call, and a
    // move to the next model does not ask them again.
    let signals =
        crate::plugins::run::before_plan(gw, caller, || plugin_input(caller, iface, headers, &bytes, call), rec)
            .await?;
    let mut plan = plan(caller, iface, headers, &bytes, call, signals.as_ref())?;
    if let Some(t) = turn.as_mut() {
        t.model = plan.model_requested.clone();
    }
    let turn = turn.map(Arc::new);
    let snap = &caller.snapshot;
    rec.event.model_requested = plan.model_requested.clone();
    plan.params.attribute(caller, rec);
    common::record_target(snap, &plan.target, rec);
    common::record_route(&plan.target, plan.route.as_ref(), rec);
    rec.event.dropped_params = plan.shaped.dropped.clone();
    rec.param_sources = plan.shaped.sources.clone();
    rec.event.stream = plan.shaped.stream;
    let route = if plan.same_dialect { "same_dialect" } else { "translated" };
    common::begin_audit(
        gw,
        caller,
        Some(&plan.target),
        &plan.params,
        "POST",
        iface.path(),
        headers,
        Some(&received),
        route,
        &plan.warnings,
        rec,
    )?;
    if let Some(a) = rec.audit.as_mut() {
        a.resolution["routing"] = route_resolution(&plan.target, plan.route.as_ref());
        if let Some(t) = &turn {
            a.resolution["stored_response"] =
                json!({ "id": t.id, "previous_response_id": t.previous, "store": t.store });
        }
    }

    // The response cache (docs/plugin-concept.md, section 15): a call answered before is
    // answered again from the kept answer, before it takes a slot or counts against a limit.
    // The gateway's own calls are never cached.
    let answering = caller.workspace.answering().filter(|b| b.bound.is_ok() && caller.internal.is_none()).cloned();
    let asked_temperature = answering.as_ref().and_then(|_| caller_temperature(iface, &bytes, &plan.top));
    let mut planned_keep = None;
    if let Some(binding) = &answering {
        planned_keep = answers_for(binding, caller, headers, &plan, asked_temperature);
        let result = match planned_keep {
            None => cache_result::BYPASS,
            Some(keep) if !keep.lookup => cache_result::REFRESH,
            Some(keep) => {
                match answers::lookup(gw, binding, &caller.key.workspace_id, caller.workspace.answers_cleared_at, &keep)
                    .await
                {
                    Found::Kept(kept, result) => {
                        if let Some(a) = rec.audit.as_mut() {
                            a.resolution["cache"] = kept.audit(result);
                        }
                        if result == cache_result::HIT {
                            rec.event.cache = result.to_owned();
                            metrics::record_response_cache(result);
                            let sse = plan.shaped.stream;
                            let mut answer = replay(caller, iface, plan, *kept, rec, turn.as_deref());
                            if let Answer::Whole { body, .. } = &mut answer {
                                if let Some(t) = &turn {
                                    *body = t.keep_whole(std::mem::take(body), sse, state::Owner::of(rec)).await;
                                }
                                if let Some(a) = rec.audit.as_mut() {
                                    a.client_chunk(body);
                                }
                            }
                            return Ok(answer);
                        }
                        result
                    }
                    Found::Nothing => cache_result::MISS,
                }
            }
        };
        rec.event.cache = result.to_owned();
        metrics::record_response_cache(result);
    }
    common::admit_slot(gw, caller, rec)?;

    // Each model routing chose, in order, until one answers or the failover policy says stop.
    // A call moves on only before anything of an answer has reached the caller.
    let failover = plan.failover();
    let several = plan.route.as_ref().is_some_and(|r| r.candidates.len() > 1);
    let deadline = failover.and_then(|f| f.deadline_ms).map(|ms| rec.timeline.start + Duration::from_millis(ms.into()));
    let mut tried: Vec<Value> = Vec::new();
    let mut up = loop {
        let target = &plan.target;
        let (mut timeout, mut attempts) =
            common::timeout_and_attempts(Some(target), target.provider.doc.default_timeout_seconds, &plan.params);
        if let Some(f) = failover.filter(|_| several) {
            attempts = f.attempts_per_target.unwrap_or(1);
            if let Some(ms) = f.per_target_timeout_ms {
                timeout = timeout.min(Duration::from_millis(ms.into()));
            }
        }
        // The caller's `x-gw-retries` caps the attempts of the whole call.
        if let Some(cap) = plan.params.retries {
            attempts = attempts.min(cap.saturating_sub(rec.event.attempts)).max(1);
        }
        if let Some(d) = deadline {
            timeout = timeout.min(d.saturating_duration_since(Instant::now()).max(Duration::from_millis(1)));
        }
        let retries = plan.params.retries;
        let may_move_on = |f: Failure, rec: &CallRecorder| {
            failover.is_some_and(|p| f.allowed_by(&p))
                && retries.is_none_or(|cap| rec.event.attempts < cap)
                && deadline.is_none_or(|d| Instant::now() < d)
        };
        let from = (target.model.doc.name.clone(), target.provider.doc.name.clone());

        let failure = match common::admit_limits(gw, caller, rec) {
            Err((e, model_specific)) => {
                // A limit that counts only this model leaves the others free.
                if model_specific && may_move_on(Failure::LimitRefused, rec) && plan.advance(caller, iface, &bytes) {
                    Failure::LimitRefused
                } else {
                    return Err(e);
                }
            }
            Ok(()) => {
                let live = target.live().cloned();
                rec.in_flight = live.as_ref().map(|s| s.begin());
                let result = send_to(
                    gw,
                    caller,
                    headers,
                    iface,
                    plan.wire,
                    target,
                    plan.url(),
                    &plan.shaped.body,
                    plan.shaped.stream,
                    timeout,
                    attempts,
                    rec,
                    None,
                )
                .await;
                let failure = Failure::of(&result);
                if let Some(s) = &live {
                    match (failure, &result) {
                        (None, Ok(up)) => s.success(up.first_byte),
                        (Some(Failure::RateLimited(wait)), _) => s.failure(now_ms(), wait),
                        (Some(_), _) => s.failure(now_ms(), None),
                        (None, Err(_)) => {}
                    }
                }
                // Without a model to move on to, the provider's own answer stands.
                match failure {
                    Some(f) if may_move_on(f, rec) && plan.advance(caller, iface, &bytes) => f,
                    _ => break result?,
                }
            }
        };
        rec.in_flight = None;
        metrics::record_failover(&from.1, &from.0, failure.name());
        tried.push(
            json!({ "model": from.0, "provider": from.1, "reason": failure.name(), "status": rec.event.http_status }),
        );
        // An outage absorbed by another model still shows on the health page and its alerts.
        if failure != Failure::LimitRefused {
            gw.live.record_health(&from.1, &from.0, false, 0.0);
        }
        rec.event.models_tried.push(from.0);
        rec.event.failovers = rec.event.failovers.saturating_add(1);
        rec.event.http_status = 0;
        common::record_target(snap, &plan.target, rec);
        rec.event.dropped_params = plan.shaped.dropped.clone();
        rec.param_sources = plan.shaped.sources.clone();
    };
    if let Some(a) = rec.audit.as_mut()
        && !tried.is_empty()
    {
        a.resolution["tried"] = json!(tried);
        a.resolution["model_served"] = json!(rec.event.model_served);
        a.resolution["provider"] = json!(rec.event.provider);
        a.resolution["dropped_params"] = json!(rec.event.dropped_params);
        a.resolution["route"] = json!(if plan.same_dialect { "same_dialect" } else { "translated" });
        a.resolution["credential"] = common::credential_json(caller, &plan.target);
    }

    // The answer is kept under the key of the model that gives it, which a failover changed.
    if let Some(binding) = answering {
        let keep = if rec.event.failovers == 0 {
            planned_keep
        } else {
            answers_for(&binding, caller, headers, &plan, asked_temperature)
        };
        if let Some(keep) = keep.filter(|k| k.store) {
            let asked = rec.timeline.wall_start;
            rec.answer.keep(gw, binding, &caller.key.workspace_id, keep, asked);
        }
    }

    let reads = plan.reads();
    let Plan { target, shaped, profile, warnings, wire, same_dialect, echo, .. } = plan;

    // --- a stream is passed on, or shaped event by event, as it arrives ---
    if up.status.is_success() && up.content_type.starts_with("text/event-stream") {
        rec.event.stream = true;
        rec.tap = Some(UsageTap::new(reads, &up.content_type).with_fields(target.provider.usage_fields.clone()));
        // The response cache reads the provider's events as they are relayed.
        if rec.answer.wants() {
            let read_as = profile.clone().unwrap_or_else(|| translate::profile_for(&target, wire, false));
            rec.answer.stream(translate::provider_stream_of(reads, wire, &read_as));
        }
        let transform: Option<Box<dyn Transform>> = match (same_dialect, wire, profile) {
            (true, Wire::OpenAi, _) if iface == Interface::Responses => {
                Some(Box::new(super::responses::Stream { framer: SseFramer::new() }))
            }
            (true, Wire::OpenAi, _) => Some(Box::new(openai::ChatStream {
                framer: SseFramer::new(),
                wants_usage: shaped.wants_usage,
                fields: target.provider.usage_fields.clone(),
            })),
            // Anthropic's own events for an Anthropic caller pass as they are; only a stream
            // that breaks gets Anthropic's own error event to end it.
            (true, Wire::Anthropic, _) => Some(Box::new(anthropic::PassThrough)),
            // Gemini's for a Gemini caller too, or their data as one JSON array.
            (true, Wire::Gemini, _) => {
                let sse = call.is_none_or(|c| c.sse);
                if !sse {
                    up.head.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                }
                Some(Box::new(super::gemini::PassThrough::new(sse)))
            }
            (false, _, profile) => {
                let profile = profile.unwrap_or_default();
                // A Gemini caller that did not ask for server-sent events reads one JSON array.
                let sse = call.is_none_or(|c| c.sse);
                let content_type = if sse { "text/event-stream" } else { "application/json" };
                up.head.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
                Some(Box::new(TranslateStream {
                    framer: SseFramer::new(),
                    reader: translate::provider_stream(wire, &profile),
                    writer: None,
                    id: turn.as_ref().map(|t| t.id.clone()),
                    iface,
                    include_usage: shaped.wants_usage,
                    sse,
                    model: target.model.doc.name.clone(),
                    created: rec.timeline.wall_start.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
                    native: (iface == Interface::Native).then(|| NativeInfo { caller: caller.clone(), warnings }),
                    echo,
                    events: Vec::new(),
                }))
            }
        };
        // A kept response: its last event waits until the turn is written.
        let transform = match (turn, transform) {
            (Some(t), Some(inner)) => Some(Box::new(state::Stored::new(inner, t)) as Box<dyn Transform>),
            (_, transform) => transform,
        };
        let idle = Duration::from_secs(gw.boot.stream_idle_seconds);
        return Ok(Answer::Stream { head: up.head, incoming: up.incoming, transform, idle, target });
    }

    // --- everything else is held, so that it can be accounted before it is answered ---
    let body = up.hold(rec).await?;
    if !up.status.is_success() {
        up.refused(&body, same_dialect, wire, &target.provider.doc.quirks.error_rules)?;
        return Ok(Answer::Whole { head: up.head, body, target, cost: None });
    }

    if same_dialect {
        let found = parse_whole(reads, &body, &target.provider.usage_fields);
        let usage = found.has_usage.then_some(found.usage);
        rec.absorb(found);
        // Read in neutral terms only for the response cache, which keeps it so.
        if rec.answer.wants()
            && let Ok(r) =
                translate::read_response_of(reads, wire, &body, &translate::profile_for(&target, wire, false))
        {
            rec.answer.whole(r);
        }
        let cost = usage.as_ref().and_then(|u| rec.cost_of(u));
        let shaped = match (wire, usage, scan(&body)) {
            // The Responses API has a place for the cost too: inside `usage`.
            (Wire::OpenAi, Some(_), Ok(top)) if reads == Dialect::OpenAiResponses => {
                super::responses::add_cost(&body, &top, cost.as_ref())
            }
            // The OpenAI dialect has a place for the cost: inside `usage`.
            (Wire::OpenAi, Some(u), Ok(top)) => openai::add_cost(&body, &top, &u, cost.as_ref()),
            // An Anthropic answer goes back as Anthropic wrote it; the cost is in a header.
            _ => None,
        };
        // A kept response goes out under the gateway's id, once it is written.
        let shaped = match &turn {
            Some(t) => Some(t.keep_whole(shaped.unwrap_or_else(|| body.clone()), false, state::Owner::of(rec)).await),
            None => shaped,
        };
        if let (Some(sent), Some(a)) = (&shaped, rec.audit.as_mut()) {
            a.client_chunk(sent);
        }
        return Ok(Answer::Whole { head: up.head, body: shaped.unwrap_or(body), target, cost });
    }

    // --- translated: read the provider's answer, write the caller's ---
    let profile = profile.unwrap_or_default();
    let response = translate::read_response(wire, &body, &profile)?;
    // The model and service tier the provider reports, read as the tap reads them.
    let reported = parse_whole(wire.dialect(), &body, &target.provider.usage_fields);
    rec.absorb(Extracted {
        usage: response.usage.unwrap_or_default(),
        has_usage: response.usage.is_some(),
        model: reported.model,
        finish_reason: Some(response.native_finish.clone()),
        cut: false,
        service_tier: reported.service_tier,
        stream_error: None,
    });
    let cost = response.usage.as_ref().and_then(|u| rec.cost_of(u));
    if rec.answer.wants() {
        rec.answer.whole(response.clone());
    }
    let stamp = Stamp {
        // The native interface answers under the gateway's own id, and so does a kept response;
        // the others keep the provider's, so that the same provider answer always gives the
        // same bytes.
        id: match &turn {
            _ if iface == Interface::Native => rec.request_id(),
            Some(t) => t.id.clone(),
            None => response.id.clone(),
        },
        model: target.model.doc.name.clone(),
        created: rec.timeline.wall_start.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()),
        cost: cost.as_ref().map(openai::cost_object),
        gateway: (iface == Interface::Native).then(|| gateway_block(caller, rec, &warnings)),
        template: None,
        echo,
    };
    let body = translate::write_response(iface, &response, &stamp);
    let body = match &turn {
        Some(t) => t.keep_whole(body, false, state::Owner::of(rec)).await,
        None => body,
    };
    if let Some(a) = rec.audit.as_mut() {
        a.client_chunk(&body);
    }
    up.head.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    Ok(Answer::Whole { head: up.head, body, target, cost })
}

// --- the response cache ------------------------------------------------------------------------

/// The temperature a caller asked for in its own request, read from the scanned body.
fn caller_temperature(iface: Interface, bytes: &[u8], top: &TopLevel) -> Option<f64> {
    let raw = match iface {
        Interface::Gemini => {
            let (config, inner) = shape::inner(bytes, top, "generationConfig")
                .or_else(|| shape::inner(bytes, top, "generation_config"))?;
            inner.raw(config, "temperature")?
        }
        Interface::OpenAi | Interface::Anthropic | Interface::Native | Interface::Responses => {
            top.raw(bytes, "temperature")?
        }
    };
    serde_json::from_slice(raw).ok()
}

/// What the workspace's response cache says about a call as planned: whether it may be answered
/// from a kept answer, whether its answer is kept, and under which key. `None`: neither.
fn answers_for(
    binding: &ActiveBinding,
    caller: &Caller,
    headers: &HeaderMap,
    plan: &Plan,
    asked_temperature: Option<f64>,
) -> Option<Keep> {
    let bound = binding.bound.as_ref().ok()?;
    let target = &plan.target;
    // Request headers the provider is given that change its answer (see `send_to`).
    let mut forwarded: Vec<(&str, String)> = Vec::new();
    if plan.wire == Wire::Anthropic {
        let betas: Vec<&str> = headers.get_all("anthropic-beta").iter().filter_map(|v| v.to_str().ok()).collect();
        if !betas.is_empty() {
            forwarded.push(("anthropic-beta", betas.join(",")));
        }
        if plan.iface == Interface::Anthropic
            && let Some(v) = headers.get("anthropic-version").and_then(|v| v.to_str().ok())
        {
            forwarded.push(("anthropic-version", v.to_owned()));
        }
    }
    let forwarded: Vec<(&str, &str)> = forwarded.iter().map(|(n, v)| (*n, v.as_str())).collect();
    let call = PlannedCall {
        workspace_id: &caller.key.workspace_id,
        key_id: &caller.key.id,
        end_user: plan.params.end_user.as_deref(),
        model_id: &target.model.id,
        upstream_id: &target.model.doc.upstream_id,
        provider_id: &target.provider.id,
        template: target.template.as_ref().map(|t| (t.name.as_str(), t.version)),
        dialect: match plan.wire {
            Wire::OpenAi if plan.reads() == Dialect::OpenAiResponses => pacod_gw_plugin::Dialect::OpenAiResponses,
            Wire::OpenAi => pacod_gw_plugin::Dialect::OpenAiChat,
            Wire::Anthropic => pacod_gw_plugin::Dialect::Anthropic,
            Wire::Gemini => pacod_gw_plugin::Dialect::Gemini,
        },
        body: &plan.shaped.body,
        headers: &forwarded,
        directive: plan.params.response_cache.unwrap_or_default(),
        asked_temperature,
    };
    bound.answers(&call)
}

/// Answers a call with a kept answer, in the caller's dialect, whole or as a stream, as if the
/// model it came from had answered now: under the call's own id on the native interface and
/// for a stored response (`turn`), with the call's time, and at no cost. The caller records
/// what the answer's body became.
fn replay(
    caller: &Caller,
    iface: Interface,
    plan: Plan,
    kept: Kept,
    rec: &mut CallRecorder,
    turn: Option<&state::Turn>,
) -> Answer {
    let Plan { target, shaped, warnings, call, echo, .. } = plan;
    rec.answer.age = Some(kept.age());
    let response = kept.response;
    let e = &mut rec.event;
    e.http_status = StatusCode::OK.as_u16();
    e.stream = shaped.stream;
    e.usage = Usage::default();
    e.usage_source = usage_source::CACHE.to_owned();
    e.finish_reason = response.native_finish.clone();
    e.model_reported = response.model.clone();
    rec.event.cache_saved_cost = response.usage.as_ref().and_then(|u| rec.cost_of(u)).and_then(|c| c.amount);
    let nothing = rec.cost_of(&Usage::default());
    let cost = nothing.as_ref().map(openai::cost_object);
    let id = match turn {
        _ if iface == Interface::Native => rec.request_id(),
        Some(t) => t.id.clone(),
        None => response.id.clone(),
    };
    let created = rec.timeline.wall_start.duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let gateway = (iface == Interface::Native).then(|| gateway_block(caller, rec, &warnings));
    let model = target.model.doc.name.clone();
    let mut head = Response::new(());
    let body = if shaped.stream {
        // A Gemini caller that did not ask for server-sent events reads one JSON array.
        let sse = call.as_ref().is_none_or(|c| c.sse);
        let template =
            (!rec.event.template.is_empty()).then(|| (rec.event.template.clone(), rec.event.template_version));
        let stamp = Stamp { id, model, created, template, echo, ..Default::default() };
        let mut writer = translate::client_stream(iface, stamp, shaped.wants_usage, sse);
        let mut out = BytesMut::new();
        for event in events_of(&response) {
            if matches!(event, Event::MessageStop { .. }) {
                writer.finalize(cost.clone(), gateway.clone());
            }
            writer.write(&event, &mut out);
        }
        writer.end(&mut out);
        let content_type = if sse { "text/event-stream" } else { "application/json" };
        head.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
        out.freeze()
    } else {
        let stamp = Stamp { id, model, created, cost, gateway, template: None, echo };
        head.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        translate::write_response(iface, &response, &stamp)
    };
    Answer::Whole { head, body, target, cost: nothing }
}

// --- token counting, speech and embeddings -------------------------------------------------------

/// The auxiliary endpoints of managed mode: one request, answered from one model; relayed as
/// the provider wrote it where the provider speaks the caller's dialect, translated otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Aux {
    /// `POST /v1/messages/count_tokens`: resolved like a message, counted by Anthropic or a
    /// Gemini model's `countTokens`, free.
    CountTokens,
    /// `POST /v1/audio/speech`: OpenAI's request shape.
    Speech,
    /// `POST /gw/v1/speech`: the native request shape.
    NativeSpeech,
    /// `POST /v1/embeddings`: OpenAI's request shape, for models of kind `embedding`.
    Embeddings,
    /// `:countTokens` on the Gemini interface: counted by a Gemini model or Anthropic, free.
    GeminiCount,
    /// `:embedContent`, `:batchEmbedContents` or `:predict` on the Gemini interface.
    GeminiEmbed(GeminiEmbed),
    /// `POST /v1/systemone`: TypeSafe's request shape, for decision models.
    SystemOne,
    /// `POST /v1/images/generations` (JSON) and `/v1/images/edits` (a form): OpenAI's request
    /// shapes, for models of kind `image`.
    Images(ImageOp),
    /// `POST /v1/audio/transcriptions` and `/v1/audio/translations`: a form, for models of
    /// kind `transcription`.
    Transcribe(AudioOp),
    /// `POST /v1/rerank` (and `/v2/rerank`, where Cohere's SDK calls): Cohere's request shape,
    /// for models of kind `rerank`.
    Rerank,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ImageOp {
    Generate,
    Edit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioOp {
    Transcription,
    /// Into English.
    Translation,
}

/// What an auxiliary endpoint answers: the provider's bytes as they arrive, read on the way by
/// `tap` where it reports usage, or a whole body the gateway wrote.
enum AuxAnswer {
    Relay { head: Response<()>, incoming: hyper::body::Incoming, tap: Option<Dialect> },
    Whole { head: Response<()>, body: Bytes },
}

impl Aux {
    fn interface(self) -> Interface {
        match self {
            Aux::CountTokens => Interface::Anthropic,
            Aux::Speech | Aux::Embeddings | Aux::Images(_) | Aux::Transcribe(_) | Aux::Rerank => Interface::OpenAi,
            // TypeSafe's SDKs know no other envelope: refusals are the gateway's own.
            Aux::NativeSpeech | Aux::SystemOne => Interface::Native,
            Aux::GeminiCount | Aux::GeminiEmbed(_) => Interface::Gemini,
        }
    }

    /// The interface the usage event names: the API's, which for System One is `decision`,
    /// as for a plugin's call to it.
    fn event_interface(self) -> &'static str {
        match self {
            Aux::SystemOne => "decision",
            other => other.interface().name(),
        }
    }

    fn path(self) -> &'static str {
        match self {
            Aux::CountTokens => "/v1/messages/count_tokens",
            Aux::Speech => "/v1/audio/speech",
            Aux::NativeSpeech => "/gw/v1/speech",
            Aux::Embeddings => "/v1/embeddings",
            Aux::GeminiCount => "/v1beta/models/{model}:countTokens",
            Aux::GeminiEmbed(GeminiEmbed::One) => "/v1beta/models/{model}:embedContent",
            Aux::GeminiEmbed(GeminiEmbed::Batch) => "/v1beta/models/{model}:batchEmbedContents",
            Aux::GeminiEmbed(GeminiEmbed::Predict) => "/v1beta/models/{model}:predict",
            Aux::SystemOne => "/v1/systemone",
            Aux::Images(ImageOp::Generate) => "/v1/images/generations",
            Aux::Images(ImageOp::Edit) => "/v1/images/edits",
            Aux::Transcribe(AudioOp::Transcription) => "/v1/audio/transcriptions",
            Aux::Transcribe(AudioOp::Translation) => "/v1/audio/translations",
            Aux::Rerank => "/v1/rerank",
        }
    }

    /// The request is a form, not JSON.
    fn multipart(self) -> bool {
        matches!(self, Aux::Images(ImageOp::Edit) | Aux::Transcribe(_))
    }

    /// Counting tokens: free, and recorded as a request.
    fn counts(self) -> bool {
        matches!(self, Aux::CountTokens | Aux::GeminiCount)
    }

    /// Whether the provider's answer is in the caller's dialect.
    fn speaks(self, wire: Wire) -> bool {
        match self {
            Aux::CountTokens => wire == Wire::Anthropic,
            Aux::Speech | Aux::Embeddings | Aux::Images(_) | Aux::Transcribe(_) => wire == Wire::OpenAi,
            // Every rerank answer is written again in one shape.
            Aux::Rerank => false,
            Aux::NativeSpeech => false,
            Aux::GeminiCount | Aux::GeminiEmbed(_) => wire == Wire::Gemini,
            // `refuses` lets only TypeSafe's providers through, which speak it.
            Aux::SystemOne => true,
        }
    }
}

pub async fn count_tokens(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    handle_aux(gw, client, req, Aux::CountTokens, None).await
}

pub async fn speech(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    handle_aux(gw, client, req, Aux::Speech, None).await
}

pub async fn native_speech(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    handle_aux(gw, client, req, Aux::NativeSpeech, None).await
}

pub async fn embeddings(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    handle_aux(gw, client, req, Aux::Embeddings, None).await
}

pub async fn images_generations(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    handle_aux(gw, client, req, Aux::Images(ImageOp::Generate), None).await
}

pub async fn images_edits(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    handle_aux(gw, client, req, Aux::Images(ImageOp::Edit), None).await
}

pub async fn transcriptions(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    handle_aux(gw, client, req, Aux::Transcribe(AudioOp::Transcription), None).await
}

pub async fn translations(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    handle_aux(gw, client, req, Aux::Transcribe(AudioOp::Translation), None).await
}

pub async fn rerank(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    handle_aux(gw, client, req, Aux::Rerank, None).await
}

/// TypeSafe's System One API for applications: a decision model answers typed questions.
pub async fn systemone(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    handle_aux(gw, client, req, Aux::SystemOne, None).await
}

/// `:countTokens` on the Gemini interface.
pub(super) async fn gemini_count(gw: Arc<Gw>, client: IpAddr, req: Request, call: PathCall) -> Response {
    handle_aux(gw, client, req, Aux::GeminiCount, Some(call)).await
}

/// `:embedContent`, `:batchEmbedContents` and `:predict` on the Gemini interface.
pub(super) async fn gemini_embed(
    gw: Arc<Gw>,
    client: IpAddr,
    req: Request,
    shape: GeminiEmbed,
    call: PathCall,
) -> Response {
    handle_aux(gw, client, req, Aux::GeminiEmbed(shape), Some(call)).await
}

async fn handle_aux(gw: Arc<Gw>, client: IpAddr, req: Request, aux: Aux, call: Option<PathCall>) -> Response {
    let started = Instant::now();
    let iface = aux.interface();
    let (parts, body) = req.into_parts();
    let trace = TraceContext::from_traceparent(parts.headers.get("traceparent").and_then(|v| v.to_str().ok()));
    let caller = match authenticate(&gw, &parts.headers, Some(client), Mode::Managed) {
        Ok(c) => c,
        Err(e) => {
            metrics::record_rejection(e.kind);
            gw.live.record_rejection(e.kind);
            gw.live.record_rejected();
            return e.response(iface.envelope(), "");
        }
    };
    let mut rec = CallRecorder::new(gw.clone(), trace);
    rec.timeline = crate::relay::Timeline::starting_at(started);
    rec.event.organisation_id = caller.key.organisation_id.clone();
    rec.event.workspace_id = caller.key.workspace_id.clone();
    rec.event.key_id = caller.key.id.clone();
    rec.set_mode("managed");
    rec.event.interface = aux.event_interface().into();
    rec.event.path = aux.path().into();
    // Speech is counted in characters and counting is free; embeddings report tokens, or are
    // estimated from their input (never as output).
    rec.usage_policy = UsagePolicy::Ignore;

    match run_aux(&gw, &caller, &parts.headers, body, aux, call.as_ref(), &mut rec).await {
        Ok((answer, target)) => {
            let (mut head, rest) = match answer {
                AuxAnswer::Relay { head, incoming, tap } => (head, Ok((incoming, tap))),
                AuxAnswer::Whole { head, body } => (head, Err(body)),
            };
            let h = head.headers_mut();
            common::stamp(h, &gw, &caller.snapshot, Some(&target), &rec);
            // A cost known before the answer is sent: speech's characters, held answers.
            let known = match &rest {
                Ok(_) => aux == Aux::Speech || aux == Aux::NativeSpeech,
                Err(_) => !aux.counts(),
            };
            let usage = if rec.event.usage_source == pacod_gw_events::usage::usage_source::PROVIDER {
                Some(rec.event.usage)
            } else {
                rec.fallback_usage
            };
            if known
                && let Some(c) = usage.as_ref().and_then(|u| rec.cost_of(u))
                && let Some(amount) = c.amount
                && let Ok(v) = HeaderValue::from_str(&format!("{amount:.6} {}", c.currency))
            {
                h.insert("x-gw-cost", v);
            }
            if let Some(a) = rec.audit.as_mut() {
                a.client_head(head.status().as_u16(), head.headers(), false);
            }
            rec.timeline.client_head();
            let (head, ()) = head.into_parts();
            match rest {
                Ok((incoming, tap)) => {
                    if let Some(d) = tap {
                        let content_type =
                            head.headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default();
                        rec.tap = Some(
                            crate::extract::UsageTap::new(d, content_type)
                                .with_fields(target.provider.usage_fields.clone()),
                        );
                    }
                    let idle = Duration::from_secs(gw.boot.stream_idle_seconds);
                    Response::from_parts(head, Body::new(RelayBody::new(incoming, rec, idle)))
                }
                Err(body) => {
                    let mut head = head;
                    head.headers.insert(CONTENT_LENGTH, HeaderValue::from(body.len()));
                    if let Some(a) = rec.audit.as_mut() {
                        a.client_chunk(&body);
                    }
                    rec.complete_from_http();
                    rec.timeline.client_chunk(Instant::now());
                    rec.finish();
                    Response::from_parts(head, Body::from(body))
                }
            }
        }
        Err(e) => common::fail(&e, iface.envelope(), rec),
    }
}

/// What the request asks for, read once for every model it may reach.
#[derive(Default)]
struct AuxRequest {
    /// Embeddings: the texts and their options.
    embed: Option<pacod_gw_dialects::embeddings::EmbedRequest>,
    /// Gemini interface, token counting: the request in neutral terms.
    counted: Option<pacod_gw_dialects::Request>,
    /// Images: what is asked for.
    image: Option<pacod_gw_dialects::images::ImageRequest>,
    /// A form: its parts, and its content type with the boundary, which the provider is sent.
    form: Option<(pacod_gw_dialects::multipart::Form, HeaderValue)>,
    /// Transcription: what is asked for.
    audio: Option<pacod_gw_dialects::audio::AudioRequest>,
    /// Rerank: what is asked for.
    rerank: Option<pacod_gw_dialects::rerank::RerankRequest>,
    /// Transcription: the format the provider is asked for instead of the caller's, so that it
    /// reports what the call is priced by (see [`transcription_format`]).
    upstream_format: Option<&'static str>,
}

/// The format to ask an OpenAI-shaped provider for instead of the caller's: `verbose_json`
/// for a model priced by the second (it says how long the audio was), `json` for a model
/// priced by tokens whose caller asked for `text` (it carries the usage). `None`: the caller's.
fn transcription_format(
    audio: &pacod_gw_dialects::audio::AudioRequest,
    pricing: &crate::relay::Pricing,
) -> Option<&'static str> {
    let crate::relay::Pricing::Priced(price) = pricing else { return None };
    if audio.stream {
        return None;
    }
    let by_second =
        price.rates.per_audio_second.is_some() || price.variants.values().any(|r| r.per_audio_second.is_some());
    match audio.response_format.as_str() {
        "json" | "text" if by_second => Some("verbose_json"),
        "text" if price.rates.input.is_some() => Some("json"),
        _ => None,
    }
}

/// A model a Gemini caller named inside the body (`generateContentRequest.model`, a batch's
/// `requests[].model`) must be the path's: the gateway serves the path's model only.
fn same_model(named: &str, call: &PathCall) -> Result<(), GwError> {
    let named = named.trim_start_matches("models/");
    if named == call.model.trim_start_matches("models/") {
        Ok(())
    } else {
        Err(GwError::invalid_request(format!(
            "the body names the model \"{named}\", the path \"{}\": they must be the same",
            call.model
        )))
    }
}

impl AuxRequest {
    fn read(
        aux: Aux,
        bytes: &Bytes,
        call: Option<&PathCall>,
        form: Option<(pacod_gw_dialects::multipart::Form, HeaderValue)>,
    ) -> Result<Self, GwError> {
        use pacod_gw_dialects::embeddings as emb;
        use pacod_gw_dialects::images;
        let mut r = AuxRequest { form, ..Default::default() };
        match (aux, call) {
            (Aux::Images(ImageOp::Generate), _) => {
                r.image = Some(images::parse_generation(bytes).map_err(translate::dialect_error)?);
            }
            (Aux::Images(ImageOp::Edit), _) => {
                let (form, _) = r.form.as_ref().ok_or_else(|| GwError::internal("an edit's form was not read"))?;
                r.image = Some(images::parse_edit(form, bytes).map_err(translate::dialect_error)?);
            }
            (Aux::Rerank, _) => {
                r.rerank = Some(pacod_gw_dialects::rerank::parse_request(bytes).map_err(translate::dialect_error)?);
            }
            (Aux::Transcribe(_), _) => {
                let (form, _) = r.form.as_ref().ok_or_else(|| GwError::internal("the form was not read"))?;
                r.audio = Some(pacod_gw_dialects::audio::parse_request(form, bytes).map_err(translate::dialect_error)?);
            }
            (Aux::Embeddings, _) => r.embed = Some(emb::parse_request(bytes).map_err(translate::dialect_error)?),
            (Aux::GeminiEmbed(shape), Some(call)) => {
                let (embed, models) = emb::parse_gemini(bytes, shape).map_err(translate::dialect_error)?;
                for m in &models {
                    same_model(m, call)?;
                }
                r.embed = Some(embed);
            }
            (Aux::GeminiCount, Some(call)) => {
                // Gemini's API also takes the whole request it would generate from.
                let v: Value = serde_json::from_slice(bytes)
                    .map_err(|e| GwError::invalid_request(format!("the request body is not JSON: {e}")))?;
                let inner = v.get("generateContentRequest").or_else(|| v.get("generate_content_request"));
                let request = match inner {
                    Some(inner) => {
                        if let Some(m) = inner.get("model").and_then(Value::as_str) {
                            same_model(m, call)?;
                        }
                        pacod_gw_dialects::gemini::parse_request(inner.to_string().as_bytes())
                    }
                    None => pacod_gw_dialects::gemini::parse_request(bytes),
                };
                r.counted = Some(request.map_err(translate::dialect_error)?);
            }
            _ => {}
        }
        Ok(r)
    }
}

impl Aux {
    /// Whether a model can serve this endpoint, and why not.
    fn refuses(self, target: &Target) -> Option<String> {
        let wire = Wire::of(&target.provider.doc);
        let model = &target.model.doc;
        match self {
            Aux::CountTokens | Aux::GeminiCount if !matches!(wire, Wire::Anthropic | Wire::Gemini) => {
                Some(format!("token counting is not available for the model \"{}\"", model.name))
            }
            Aux::Embeddings | Aux::GeminiEmbed(_) if model.kind != ModelKind::Embedding => {
                Some(format!("the model \"{}\" is not an embedding model", model.name))
            }
            Aux::Embeddings | Aux::GeminiEmbed(_) if wire == Wire::Anthropic => {
                Some(format!("Anthropic makes no embeddings: the model \"{}\"", model.name))
            }
            Aux::Speech | Aux::NativeSpeech if model.kind != ModelKind::Speech || wire != Wire::OpenAi => {
                Some(format!("the model \"{}\" is not a speech model", model.name))
            }
            Aux::SystemOne if model.kind != ModelKind::Decision || target.provider.doc.family != Family::Typesafe => {
                Some(format!("the model \"{}\" is not a decision model", model.name))
            }
            Aux::Images(_) if model.kind != ModelKind::Image => {
                Some(format!("the model \"{}\" is not an image model", model.name))
            }
            Aux::Images(_) if wire == Wire::Anthropic => {
                Some(format!("Anthropic makes no images: the model \"{}\"", model.name))
            }
            Aux::Rerank if model.kind != ModelKind::Rerank || wire != Wire::OpenAi => {
                Some(format!("the model \"{}\" is not a rerank model", model.name))
            }
            Aux::Transcribe(_) if model.kind != ModelKind::Transcription || wire == Wire::Anthropic => {
                Some(format!("the model \"{}\" is not a transcription model", model.name))
            }
            _ => None,
        }
    }

    /// The provider calls that serve this request on `target`: its URL and body, one call or,
    /// for Gemini embeddings, one per batch.
    fn calls(
        self,
        target: &Target,
        bytes: &Bytes,
        top: &TopLevel,
        asked: &AuxRequest,
    ) -> Result<Vec<(String, Bytes)>, GwError> {
        use pacod_gw_dialects::embeddings as emb;
        let wire = Wire::of(&target.provider.doc);
        let doc = &target.provider.doc;
        let model = target.model.doc.upstream_id.trim_start_matches("models/");
        let gemini_url = |method: &str| format!("{}/models/{model}:{method}", doc.base_url.trim_end_matches('/'));
        let json_body = |v: &Value| Bytes::from(v.to_string());
        let vertex = target.provider.vertex.is_some();
        // The request in neutral terms, as the provider half writes it for this model.
        let written = |request: &pacod_gw_dialects::Request| {
            let profile = translate::profile_for(target, wire, false);
            let mut notes = pacod_gw_dialects::canonical::Notes::default();
            match wire {
                Wire::Gemini => pacod_gw_dialects::gemini::build_request(request, &profile, &mut notes),
                Wire::Anthropic => pacod_gw_dialects::anthropic::build_request(request, &profile, &mut notes),
                Wire::OpenAi => pacod_gw_dialects::openai::build_request(request, &profile, &mut notes),
            }
            .map_err(translate::dialect_error)
        };
        match (self, wire) {
            // Counting for a Gemini model: the request as Gemini would generate from it.
            (Aux::CountTokens, Wire::Gemini) => {
                let request = pacod_gw_dialects::anthropic::parse_request(bytes).map_err(translate::dialect_error)?;
                let body = emb::gemini_count_body(&written(&request)?, model, vertex);
                Ok(vec![(gemini_url("countTokens"), json_body(&body))])
            }
            (Aux::GeminiCount, Wire::Gemini | Wire::Anthropic) => {
                let request = asked.counted.as_ref().ok_or_else(|| GwError::internal("a count was not read"))?;
                let generate = written(request)?;
                if wire == Wire::Gemini {
                    let body = emb::gemini_count_body(&generate, model, vertex);
                    Ok(vec![(gemini_url("countTokens"), json_body(&body))])
                } else {
                    let body = emb::anthropic_count_body(&generate);
                    Ok(vec![(doc.managed_url("v1/messages/count_tokens"), json_body(&body))])
                }
            }
            (Aux::Embeddings | Aux::GeminiEmbed(_), Wire::Gemini) => {
                let embed =
                    asked.embed.as_ref().ok_or_else(|| GwError::internal("an embeddings request was not read"))?;
                if vertex {
                    Ok(emb::vertex_predictions(model, embed)
                        .iter()
                        .map(|b| (gemini_url("predict"), json_body(b)))
                        .collect())
                } else {
                    Ok(emb::gemini_batches(model, embed)
                        .iter()
                        .map(|b| (gemini_url("batchEmbedContents"), json_body(b)))
                        .collect())
                }
            }
            (Aux::GeminiEmbed(_), _) => {
                let embed =
                    asked.embed.as_ref().ok_or_else(|| GwError::internal("an embeddings request was not read"))?;
                let body = emb::openai_request(&target.model.doc.upstream_id, embed);
                Ok(vec![(doc.managed_url("embeddings"), json_body(&body))])
            }
            (Aux::GeminiCount, Wire::OpenAi) => Err(GwError::internal("token counting reached an OpenAI-shaped model")),
            (Aux::Images(_), Wire::Gemini) => {
                use pacod_gw_dialects::images;
                let image = asked.image.as_ref().ok_or_else(|| GwError::internal("an image request was not read"))?;
                // Imagen makes all the images in one call; a Gemini image model one per call.
                if model.starts_with("imagen") {
                    let body = images::imagen_request(image).map_err(translate::dialect_error)?;
                    Ok(vec![(gemini_url("predict"), json_body(&body))])
                } else {
                    let body = json_body(&images::gemini_request(image).map_err(translate::dialect_error)?);
                    Ok((0..image.n).map(|_| (gemini_url("generateContent"), body.clone())).collect())
                }
            }
            (Aux::Rerank, _) => {
                let r = asked.rerank.as_ref().ok_or_else(|| GwError::internal("a rerank request was not read"))?;
                let (body, path) = pacod_gw_dialects::rerank::build(r, rerank_shape(&target.provider.doc), model);
                Ok(vec![(doc.managed_url(path), json_body(&body))])
            }
            (Aux::Transcribe(op), Wire::Gemini) => {
                use pacod_gw_dialects::audio;
                let a = asked.audio.as_ref().ok_or_else(|| GwError::internal("a transcription was not read"))?;
                if a.file.len() > audio::GEMINI_INLINE_LIMIT {
                    return Err(GwError::too_large(audio::GEMINI_INLINE_LIMIT));
                }
                let body =
                    audio::gemini_request(a, bytes, op == AudioOp::Translation).map_err(translate::dialect_error)?;
                Ok(vec![(gemini_url("generateContent"), json_body(&body))])
            }
            (Aux::Transcribe(op), _) => {
                use pacod_gw_dialects::multipart::Edit;
                let (form, _) = asked.form.as_ref().ok_or_else(|| GwError::internal("the form was not read"))?;
                let upstream = target.model.doc.upstream_id.as_bytes();
                let mut edits = vec![("model", Edit::Set(upstream))];
                if let Some(f) = asked.upstream_format {
                    edits.push(("response_format", Edit::Set(f.as_bytes())));
                }
                let path = if op == AudioOp::Translation { "audio/translations" } else { "audio/transcriptions" };
                Ok(vec![(doc.managed_url(path), form.rewrite(bytes, &edits))])
            }
            // A form keeps every byte but its model's.
            (Aux::Images(ImageOp::Edit), _) => {
                let (form, _) = asked.form.as_ref().ok_or_else(|| GwError::internal("an edit's form was not read"))?;
                let upstream = target.model.doc.upstream_id.as_bytes();
                let out = form.rewrite(bytes, &[("model", pacod_gw_dialects::multipart::Edit::Set(upstream))]);
                Ok(vec![(doc.managed_url("images/edits"), out)])
            }
            // The provider's own dialect: the body as the caller wrote it, with the model's id.
            _ => {
                let mut patch = Patch::new();
                patch.set("model", &json!(target.model.doc.upstream_id));
                patch.remove("gateway");
                if self == Aux::Embeddings {
                    patch.remove("provider_options");
                }
                if self == Aux::NativeSpeech
                    && let Some(format) = top.get("format")
                {
                    // The native name of the field; the provider knows it as `response_format`.
                    patch.push("response_format", Op::Set(bytes.slice(format.value.clone())));
                    patch.remove("format");
                }
                let out = apply(bytes, top, &patch)
                    .map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?
                    .to_bytes();
                let path = match self {
                    Aux::CountTokens => "v1/messages/count_tokens",
                    Aux::Speech | Aux::NativeSpeech => "audio/speech",
                    Aux::SystemOne => "systemone",
                    Aux::Images(_) => "images/generations",
                    _ => "embeddings",
                };
                Ok(vec![(doc.managed_url(path), out)])
            }
        }
    }
}

/// Token counting, speech and embeddings: one provider call per model (Gemini's embeddings one
/// per batch), through the models routing chose for a template with several, moving on before
/// the first byte as the scheme's failover policy allows (concept section 7.5).
async fn run_aux(
    gw: &Arc<Gw>,
    caller: &Caller,
    headers: &HeaderMap,
    body: Body,
    aux: Aux,
    call: Option<&PathCall>,
    rec: &mut CallRecorder,
) -> Result<(AuxAnswer, Target), GwError> {
    let iface = aux.interface();
    let bytes = read_body(gw, caller, headers, body, rec).await?;
    // A form is read for its parts; its gateway parameters come from the headers only.
    let form = match aux.multipart() {
        true => {
            let ct = headers.get(CONTENT_TYPE).cloned().unwrap_or(HeaderValue::from_static(""));
            let parsed = pacod_gw_dialects::multipart::Form::parse(ct.to_str().unwrap_or_default(), &bytes)
                .map_err(translate::dialect_error)?;
            Some((parsed, ct))
        }
        false => None,
    };
    let top = match &form {
        Some(_) => scan(b"{}").map_err(GwError::internal)?,
        None => scan(&bytes)
            .map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?,
    };
    let mut params = GatewayParams::from_headers(headers)?;
    if let Some(e) = top.get("gateway") {
        params.merge_object(&bytes[e.value.clone()])?;
    }
    params.attribute(caller, rec);

    let model = match (call, &form) {
        (Some(c), _) => c.model.clone(),
        (None, Some((f, _))) => f.text(&bytes, "model").unwrap_or_default().to_owned(),
        (None, None) => top.str(&bytes, "model").unwrap_or_default(),
    };
    if model.is_empty() && params.template.is_none() {
        return Err(GwError::invalid_request("model is required: a model id or @template"));
    }
    rec.event.model_requested =
        if model.is_empty() { format!("@{}", params.template.as_deref().unwrap_or_default()) } else { model.clone() };
    let snap = &caller.snapshot;
    let req = ResolveRequest { model: &model, template: params.template.as_deref(), provider: None, families: None };
    let (mut target, mut route) = resolve_routed(
        snap,
        &caller.workspace,
        &caller.key,
        &req,
        &route_input(&params, iface.name(), false, bytes.len(), None),
    )?;
    // Models of the pool that cannot serve this endpoint are passed over.
    let advance = |target: &mut Target, route: &mut Option<RouteChoice>| -> bool {
        let (Some(route), Some(template)) = (route.as_mut(), target.template.clone()) else { return false };
        let req = ResolveRequest { model: "", template: Some(&template.name), provider: None, families: None };
        while let Some(next) = next_target(snap, &caller.workspace, &caller.key, &req, &template, route) {
            match aux.refuses(&next) {
                Some(why) => route.skipped.push((next.pool_index.unwrap_or_default(), why)),
                None => {
                    *target = next;
                    return true;
                }
            }
        }
        false
    };
    if let Some(why) = aux.refuses(&target)
        && !advance(&mut target, &mut route)
    {
        return Err(if aux.counts() { GwError::unsupported_feature(why) } else { GwError::invalid_request(why) });
    }
    common::record_target(snap, &target, rec);
    common::record_route(&target, route.as_ref(), rec);

    let mut asked = AuxRequest::read(aux, &bytes, call, form)?;
    // Images: priced per image where the provider reports no tokens (DALL·E, Imagen).
    if let Some(i) = &asked.image {
        rec.fallback_usage = Some(pacod_gw_events::Usage { images: i.n, ..Default::default() });
    }
    // Embeddings: an estimate of the input's tokens for a provider that reports none.
    if let Some(e) = &asked.embed {
        let characters: usize = e.inputs.iter().map(|t| t.chars().count()).sum();
        rec.fallback_usage =
            Some(pacod_gw_events::Usage { input_tokens: characters.div_ceil(4) as u64, ..Default::default() });
    }
    // Speech is priced by the characters of the text, whichever model speaks it.
    let input = top.str(&bytes, "input").unwrap_or_default();
    if aux == Aux::Speech || aux == Aux::NativeSpeech {
        if input.is_empty() {
            return Err(GwError::invalid_request("input is required: the text to speak"));
        }
        rec.absorb(Extracted {
            usage: pacod_gw_events::Usage { characters: input.chars().count() as u64, ..Default::default() },
            has_usage: true,
            ..Default::default()
        });
    }
    // A form's files are not kept in the audit record, only what they are.
    let audited = match &asked.form {
        Some((f, _)) => crate::relay::audit::form_summary(f, &bytes),
        None => bytes.clone(),
    };
    common::begin_audit(
        gw,
        caller,
        Some(&target),
        &params,
        "POST",
        aux.path(),
        headers,
        Some(&audited),
        "auxiliary",
        &[],
        rec,
    )?;
    if let Some(a) = rec.audit.as_mut() {
        a.resolution["routing"] = route_resolution(&target, route.as_ref());
    }
    common::admit_slot(gw, caller, rec)?;

    let failover = target.template.as_ref().and_then(|t| Some(route.as_ref()?.action(t).failover));
    loop {
        // The calls for this model: its own id in the caller's dialect, or translated.
        // Rerank: without the provider's count, search units where the price has them (one
        // per query and up to 100 documents, as Cohere counts them), else tokens.
        if let Some(r) = &asked.rerank {
            let by_unit = matches!(&rec.pricing, crate::relay::Pricing::Priced(p) if p.rates.per_search_unit.is_some());
            rec.fallback_usage = Some(if by_unit {
                pacod_gw_events::Usage { search_units: r.documents.len().div_ceil(100) as u64, ..Default::default() }
            } else {
                pacod_gw_events::Usage { input_tokens: r.characters().div_ceil(4) as u64, ..Default::default() }
            });
        }
        // A transcription asks for what it is priced by.
        if let Some(a) = &asked.audio {
            let same = Wire::of(&target.provider.doc) == Wire::OpenAi;
            asked.upstream_format = if same { transcription_format(a, &rec.pricing) } else { None };
        }
        let mut calls = aux.calls(&target, &bytes, &top, &asked)?.into_iter();
        let (url, out) = calls.next().ok_or_else(|| GwError::internal("no provider call for the request"))?;
        if aux.counts() {
            // Counting costs nothing and uses no tokens: it is recorded as a request.
            rec.pricing = crate::relay::Pricing::Unknown;
        }
        let wire = Wire::of(&target.provider.doc);
        // A form goes to a provider of its own dialect as the caller sent it, boundary and all.
        let content_type = asked.form.as_ref().filter(|_| wire == Wire::OpenAi).map(|(_, ct)| ct.clone());
        // An image's size and quality may have prices of their own.
        if let (Some(i), crate::relay::Pricing::Priced(p)) = (&asked.image, &rec.pricing) {
            rec.price_variant = pacod_gw_dialects::images::price_variant(i, |v| p.variants.contains_key(v));
        }
        let from = (target.model.doc.name.clone(), target.provider.doc.name.clone());
        let retries = params.retries;
        let may_move_on = |f: Failure, rec: &CallRecorder| {
            failover.is_some_and(|p| f.allowed_by(&p)) && retries.is_none_or(|cap| rec.event.attempts < cap)
        };

        let failure = match common::admit_limits(gw, caller, rec) {
            Err((e, model_specific)) => {
                if model_specific && may_move_on(Failure::LimitRefused, rec) && advance(&mut target, &mut route) {
                    Failure::LimitRefused
                } else {
                    return Err(e);
                }
            }
            Ok(()) => {
                let (mut timeout, mut attempts) =
                    common::timeout_and_attempts(Some(&target), target.provider.doc.default_timeout_seconds, &params);
                // With several models, each gets the attempts and the time the scheme gives one.
                if let Some(f) = failover.filter(|_| route.as_ref().is_some_and(|r| r.candidates.len() > 1)) {
                    attempts = f.attempts_per_target.unwrap_or(1);
                    if let Some(ms) = f.per_target_timeout_ms {
                        timeout = timeout.min(Duration::from_millis(ms.into()));
                    }
                }
                if let Some(cap) = params.retries {
                    attempts = attempts.min(cap.saturating_sub(rec.event.attempts)).max(1);
                }
                let result = match credential_for(caller, &target) {
                    Ok(_) => {
                        send_to(
                            gw,
                            caller,
                            headers,
                            iface,
                            wire,
                            &target,
                            url,
                            &out,
                            false,
                            timeout,
                            attempts,
                            rec,
                            content_type.as_ref(),
                        )
                        .await
                    }
                    Err(e) => Err(e),
                };
                let failure = Failure::of(&result);
                if let Some(s) = target.live() {
                    match (failure, &result) {
                        (None, Ok(up)) => s.success(up.first_byte),
                        (Some(Failure::RateLimited(wait)), _) => s.failure(now_ms(), wait),
                        (Some(_), _) => s.failure(now_ms(), None),
                        (None, Err(_)) => {}
                    }
                }
                match failure {
                    Some(f) if may_move_on(f, rec) && advance(&mut target, &mut route) => f,
                    _ => {
                        let mut up = result?;
                        if !up.status.is_success() {
                            let body = up.hold(rec).await?;
                            up.refused(&body, aux.speaks(wire), wire, &target.provider.doc.quirks.error_rules)?;
                            return Err(GwError::upstream_refused(up.status, provider_message(&body)));
                        }
                        let answer = match (aux, wire) {
                            (Aux::CountTokens, Wire::Gemini) => {
                                let body = up.hold(rec).await?;
                                let answer = pacod_gw_dialects::embeddings::anthropic_count_answer(&body)
                                    .map_err(translate::dialect_error)?;
                                whole(up.head, &answer)
                            }
                            (Aux::GeminiCount, Wire::Anthropic) => {
                                let body = up.hold(rec).await?;
                                let answer = pacod_gw_dialects::embeddings::gemini_count_answer(&body)
                                    .map_err(translate::dialect_error)?;
                                whole(up.head, &answer)
                            }
                            (Aux::Embeddings | Aux::GeminiEmbed(_), Wire::Gemini) => {
                                // The rest of the batches go to the same model, one after another.
                                let mut bodies = vec![up.hold(rec).await?];
                                for (url, out) in calls {
                                    let mut next = send_to(
                                        gw, caller, headers, iface, wire, &target, url, &out, false, timeout, attempts,
                                        rec, None,
                                    )
                                    .await?;
                                    let body = next.hold(rec).await?;
                                    if !next.status.is_success() {
                                        return Err(GwError::upstream_refused(next.status, provider_message(&body)));
                                    }
                                    bodies.push(body);
                                }
                                let e =
                                    asked.embed.as_ref().ok_or_else(|| GwError::internal("no embeddings request"))?;
                                let (vectors, counts) = gemini_vectors(&target, e, &bodies, rec)?;
                                let answer = match aux {
                                    Aux::GeminiEmbed(shape) => {
                                        pacod_gw_dialects::embeddings::gemini_answer(shape, &vectors, &counts)
                                    }
                                    _ => {
                                        let tokens = rec_input_tokens(rec);
                                        pacod_gw_dialects::embeddings::openai_answer(
                                            &target.model.doc.name,
                                            &vectors,
                                            e.base64,
                                            tokens,
                                        )
                                    }
                                };
                                whole(up.head, &answer)
                            }
                            // An OpenAI-shaped provider's vectors for a Gemini caller.
                            (Aux::GeminiEmbed(shape), _) => {
                                let body = up.hold(rec).await?;
                                let (vectors, tokens) = pacod_gw_dialects::embeddings::read_openai(&body)
                                    .map_err(translate::dialect_error)?;
                                if let Some(tokens) = tokens {
                                    rec.absorb(Extracted {
                                        usage: pacod_gw_events::Usage { input_tokens: tokens, ..Default::default() },
                                        has_usage: true,
                                        ..Default::default()
                                    });
                                }
                                whole(up.head, &pacod_gw_dialects::embeddings::gemini_answer(shape, &vectors, &[]))
                            }
                            (Aux::Embeddings, _) => AuxAnswer::Relay {
                                head: up.head,
                                incoming: up.incoming,
                                tap: Some(Dialect::OpenAiEmbeddings),
                            },
                            (Aux::Images(_), Wire::Gemini) => {
                                use pacod_gw_dialects::images;
                                // The rest of the images from the same model, one after another.
                                let mut bodies = vec![up.hold(rec).await?];
                                for (url, out) in calls {
                                    let mut next = send_to(
                                        gw, caller, headers, iface, wire, &target, url, &out, false, timeout, attempts,
                                        rec, None,
                                    )
                                    .await?;
                                    let body = next.hold(rec).await?;
                                    if !next.status.is_success() {
                                        return Err(GwError::upstream_refused(next.status, provider_message(&body)));
                                    }
                                    bodies.push(body);
                                }
                                let imagen =
                                    target.model.doc.upstream_id.trim_start_matches("models/").starts_with("imagen");
                                let mut made = Vec::new();
                                let mut usage: Option<pacod_gw_events::Usage> = None;
                                for body in &bodies {
                                    if imagen {
                                        made.extend(images::read_imagen(body).map_err(translate::dialect_error)?);
                                    } else {
                                        let (found, used) =
                                            images::read_gemini(body).map_err(translate::dialect_error)?;
                                        made.extend(found);
                                        if let Some(u) = used {
                                            usage = Some(add_usage(usage.unwrap_or_default(), &u));
                                        }
                                    }
                                }
                                if let Some(u) = usage {
                                    rec.absorb(Extracted { usage: u, has_usage: true, ..Default::default() });
                                }
                                let cost = usage
                                    .or(rec.fallback_usage)
                                    .and_then(|u| rec.cost_of(&u))
                                    .map(|c| openai::cost_object(&c));
                                let created = rec
                                    .timeline
                                    .wall_start
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map_or(0, |d| d.as_secs());
                                whole(up.head, &images::openai_answer(created, &made, usage.as_ref(), cost.as_ref()))
                            }
                            (Aux::Rerank, _) => {
                                use pacod_gw_dialects::rerank;
                                let body = up.hold(rec).await?;
                                let r = asked.rerank.as_ref().ok_or_else(|| GwError::internal("no rerank request"))?;
                                let (ranked, usage) = rerank::read(&body, rerank_shape(&target.provider.doc))
                                    .map_err(translate::dialect_error)?;
                                if let Some(u) = usage {
                                    rec.absorb(Extracted { usage: u, has_usage: true, ..Default::default() });
                                }
                                let cost = usage
                                    .or(rec.fallback_usage)
                                    .and_then(|u| rec.cost_of(&u))
                                    .map(|c| openai::cost_object(&c));
                                whole(
                                    up.head,
                                    &rerank::answer(&rec.request_id(), r, &ranked, usage.as_ref(), cost.as_ref()),
                                )
                            }
                            (Aux::Transcribe(_), Wire::Gemini) => {
                                use pacod_gw_dialects::audio;
                                let body = up.hold(rec).await?;
                                let a = asked.audio.as_ref().ok_or_else(|| GwError::internal("no transcription"))?;
                                let (text, usage) = audio::read_gemini(&body).map_err(translate::dialect_error)?;
                                if let Some(u) = usage {
                                    rec.absorb(Extracted { usage: u, has_usage: true, ..Default::default() });
                                }
                                let (out, kind) = audio::answer(&text, &a.response_format, usage.as_ref(), None);
                                whole_as(up.head, out, kind)
                            }
                            // The provider was asked for another format: the caller's is written
                            // from it, with what it says the call used.
                            (Aux::Transcribe(_), _) if asked.upstream_format.is_some() => {
                                use pacod_gw_dialects::audio;
                                let body = up.hold(rec).await?;
                                let a = asked.audio.as_ref().ok_or_else(|| GwError::internal("no transcription"))?;
                                let found =
                                    parse_whole(Dialect::OpenAiTranscription, &body, &target.provider.usage_fields);
                                let usage = found.has_usage.then_some(found.usage);
                                rec.absorb(found);
                                let written = match asked.upstream_format {
                                    Some("verbose_json") => audio::reshape(&body, &a.response_format),
                                    _ => serde_json::from_slice::<Value>(&body).ok().and_then(|v| {
                                        let text = v.get("text")?.as_str()?.to_owned();
                                        Some(audio::answer(&text, &a.response_format, usage.as_ref(), None))
                                    }),
                                };
                                let (out, kind) = written.ok_or_else(|| {
                                    GwError::upstream_error(
                                        "the provider's transcription is not in the format asked for",
                                    )
                                })?;
                                whole_as(up.head, out, kind)
                            }
                            (Aux::Transcribe(_), _) => AuxAnswer::Relay {
                                head: up.head,
                                incoming: up.incoming,
                                tap: Some(Dialect::OpenAiTranscription),
                            },
                            (Aux::Images(_), _) => AuxAnswer::Relay {
                                head: up.head,
                                incoming: up.incoming,
                                tap: Some(Dialect::OpenAiImages),
                            },
                            (Aux::SystemOne, _) => {
                                AuxAnswer::Relay { head: up.head, incoming: up.incoming, tap: Some(Dialect::SystemOne) }
                            }
                            _ => AuxAnswer::Relay { head: up.head, incoming: up.incoming, tap: None },
                        };
                        return Ok((answer, target));
                    }
                }
            }
        };
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

/// Two usages of one call added up (several provider calls for one request).
fn add_usage(a: pacod_gw_events::Usage, b: &pacod_gw_events::Usage) -> pacod_gw_events::Usage {
    pacod_gw_events::Usage {
        input_tokens: a.input_tokens + b.input_tokens,
        cache_read_tokens: a.cache_read_tokens + b.cache_read_tokens,
        output_tokens: a.output_tokens + b.output_tokens,
        reasoning_tokens: a.reasoning_tokens + b.reasoning_tokens,
        audio_input_tokens: a.audio_input_tokens + b.audio_input_tokens,
        image_input_tokens: a.image_input_tokens + b.image_input_tokens,
        audio_output_tokens: a.audio_output_tokens + b.audio_output_tokens,
        image_output_tokens: a.image_output_tokens + b.image_output_tokens,
        ..a
    }
}

/// The rerank API a provider speaks: Cohere's own, or what its quirks say.
fn rerank_shape(provider: &Provider) -> pacod_gw_dialects::rerank::Shape {
    use pacod_gw_dialects::rerank::Shape;
    match (provider.family, provider.quirks.rerank_api) {
        (Family::Cohere, _) => Shape::Cohere,
        (_, crate::model::RerankApi::Voyage) => Shape::Voyage,
        (_, crate::model::RerankApi::Jina) => Shape::Jina,
    }
}

/// A whole answer of another type with the provider's head.
fn whole_as(mut head: Response<()>, body: Vec<u8>, content_type: &'static str) -> AuxAnswer {
    head.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static(content_type));
    AuxAnswer::Whole { head, body: Bytes::from(body) }
}

/// A whole JSON answer with the provider's head.
fn whole(mut head: Response<()>, body: &Value) -> AuxAnswer {
    head.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    AuxAnswer::Whole { head, body: Bytes::from(body.to_string()) }
}

/// The input tokens to report: the provider's count where it gave one, else the estimate.
fn rec_input_tokens(rec: &CallRecorder) -> u64 {
    if rec.event.usage_source == pacod_gw_events::usage::usage_source::PROVIDER {
        rec.event.usage.input_tokens
    } else {
        rec.fallback_usage.as_ref().map_or(0, |u| u.input_tokens)
    }
}

/// The vectors of Gemini's batches (or Vertex's predictions), in order, and the tokens of each
/// input where Vertex counted them. Gemini's API reports none, so the estimate from the input
/// stands there.
fn gemini_vectors(
    target: &Target,
    e: &pacod_gw_dialects::embeddings::EmbedRequest,
    bodies: &[Bytes],
    rec: &mut CallRecorder,
) -> Result<(Vec<Vec<f32>>, Vec<u64>), GwError> {
    use pacod_gw_dialects::embeddings as emb;
    let mut vectors = Vec::with_capacity(e.inputs.len());
    let mut counts = Vec::new();
    for body in bodies {
        if target.provider.vertex.is_some() {
            let (v, c) = emb::read_vertex(body).map_err(translate::dialect_error)?;
            vectors.extend(v);
            counts.extend(c);
        } else {
            vectors.extend(emb::read_gemini(body).map_err(translate::dialect_error)?);
        }
    }
    if vectors.len() != e.inputs.len() {
        return Err(GwError::upstream_error(format!(
            "the provider answered {} embeddings for {} inputs",
            vectors.len(),
            e.inputs.len()
        )));
    }
    let tokens: u64 = counts.iter().sum();
    if tokens > 0 {
        rec.absorb(Extracted {
            usage: pacod_gw_events::Usage { input_tokens: tokens, ..Default::default() },
            has_usage: true,
            ..Default::default()
        });
    } else {
        counts.clear();
    }
    Ok((vectors, counts))
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
    /// The id the answer goes out under in place of the provider's: a stored response's.
    id: Option<String>,
    iface: Interface,
    include_usage: bool,
    /// Gemini interface: server-sent events, or one JSON array.
    sse: bool,
    /// The model name the caller sees.
    model: String,
    created: u64,
    native: Option<NativeInfo>,
    /// Responses interface: the request members its answer repeats.
    echo: Option<Value>,
    events: Vec<Event>,
}

impl TranslateStream {
    fn writer(&mut self, first: Option<&Event>, rec: &CallRecorder) -> &mut Box<dyn ClientStream> {
        self.writer.get_or_insert_with(|| {
            let id = match (first, self.iface) {
                (_, Interface::Native) => rec.request_id(),
                _ if self.id.is_some() => self.id.clone().unwrap_or_default(),
                (Some(Event::MessageStart { id, .. }), _) => id.clone(),
                _ => String::new(),
            };
            let template =
                (!rec.event.template.is_empty()).then(|| (rec.event.template.clone(), rec.event.template_version));
            let stamp = Stamp {
                id,
                model: self.model.clone(),
                created: self.created,
                template,
                echo: self.echo.clone(),
                ..Default::default()
            };
            translate::client_stream(self.iface, stamp, self.include_usage, self.sse)
        })
    }

    fn drain(&mut self, rec: &mut CallRecorder) -> Bytes {
        let mut out = BytesMut::new();
        for event in std::mem::take(&mut self.events) {
            let stop = matches!(event, Event::MessageStop { .. });
            let native = self.native.as_ref().filter(|_| stop).map(|n| gateway_block(&n.caller, rec, &n.warnings));
            let writer = self.writer(Some(&event), rec);
            if let Event::MessageStop { usage, .. } = &event {
                // The cost is known now, and so is everything the gateway block reports.
                let cost = usage.as_ref().and_then(|u| rec.cost_of(u)).map(|c| openai::cost_object(&c));
                writer.finalize(cost, native);
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
        let mut out = BytesMut::from(&self.drain(rec)[..]);
        self.writer(None, rec).end(&mut out);
        out.freeze()
    }

    fn error(&mut self, kind: &str, message: &str, rec: &mut CallRecorder) -> Bytes {
        let kind = if kind == "upstream_timeout" { "upstream_timeout" } else { "upstream_error" };
        self.events.push(Event::Error { kind: kind.to_owned(), message: message.to_owned() });
        let mut out = BytesMut::from(&self.drain(rec)[..]);
        self.writer(None, rec).end(&mut out);
        out.freeze()
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
        let provider = |family, managed_api| Provider {
            name: "p".into(),
            display_name: String::new(),
            family,
            base_url: "https://x".into(),
            proxy_base_url: None,
            auth: None,
            managed_api,
            proxy_paths: vec![],
            default_timeout_seconds: 180,
            quirks: Default::default(),
            holidays: None,
            platform: Default::default(),
            status: Default::default(),
        };
        assert_eq!(Wire::of(&provider(Family::Gemini, ManagedApi::OpenaiCompatible)), Wire::OpenAi);
        assert_eq!(Wire::of(&provider(Family::Gemini, ManagedApi::Native)), Wire::Gemini);
        assert_eq!(Wire::of(&provider(Family::Ollama, ManagedApi::OpenaiCompatible)), Wire::OpenAi);
        assert_eq!(Wire::of(&provider(Family::Anthropic, ManagedApi::OpenaiCompatible)), Wire::Anthropic);
        assert_eq!((Interface::Native.path(), Interface::Native.envelope()), ("/gw/v1/chat", Envelope::Gateway));
        assert_eq!(Interface::Anthropic.envelope(), Envelope::Anthropic);
    }
}
