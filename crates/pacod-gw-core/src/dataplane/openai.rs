//! The OpenAI-compatible interface in managed mode (concept sections 4.3 and 9.3).
//!
//! This is the same-dialect path: the provider speaks the OpenAI dialect too, so the
//! conversation content is forwarded as the bytes the caller sent and only the fields the
//! gateway governs are rewritten. Models of the Anthropic family are reached through the
//! translating adapter instead.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{ConnectInfo, Path, Request, State};
use axum::http::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, RETRY_AFTER};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::Response;
use bytes::{Bytes, BytesMut};
use http_body_util::BodyExt;
use memchr::memmem;
use pacod_gw_events::Usage;
use serde_json::{Map, Value, json};

use super::Gw;
use super::auth::{Caller, authenticate, identify};
use super::common::{self, GatewayParams, SendPlan};
use super::errors::{Envelope, GwError};
use super::shape;
use crate::cost::Costed;
use crate::extract::{Dialect, UsageTap, openai_chat_usage, parse_whole};
use crate::json::{Kind, Op, Patch, TopLevel, apply, scan};
use crate::model::{AuthScheme, Family, Mode, ModelKind, PromptCacheMode, Quirks, UnsupportedParams, UsagePolicy};
use crate::params::{MergeContext, merge};
use crate::relay::{CallRecorder, RelayBody, Transform};
use crate::resolve::{ResolveRequest, Target, callable_models, resolve};
use crate::snapshot::CredentialOwner;
use crate::sse::{SseFramer, write_event};
use crate::telemetry::metrics;
use crate::telemetry::trace::TraceContext;

/// The provider families the OpenAI-compatible adapter serves without translation. Gemini
/// and Ollama are reached through their OpenAI-compatible endpoints.
const SAME_DIALECT: &[Family] = &[Family::Openai, Family::Gemini, Family::Ollama];

/// A non-streamed answer is held to add the cost to it. Larger answers are passed on as they are.
const MAX_HELD_RESPONSE: usize = 32 << 20;

/// How much of a provider's error body is read.
const MAX_ERROR_BODY: usize = 1 << 20;

fn render(v: &Value) -> Bytes {
    Bytes::from(serde_json::to_vec(v).expect("a JSON value always serialises"))
}

pub async fn chat_completions(
    State(gw): State<Arc<Gw>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
) -> Response {
    let started = Instant::now();
    let (parts, body) = req.into_parts();
    let trace = TraceContext::from_traceparent(parts.headers.get("traceparent").and_then(|v| v.to_str().ok()));

    let caller = match authenticate(&gw, &parts.headers, Some(peer.ip()), Mode::Managed) {
        Ok(c) => c,
        Err(e) => {
            metrics::record_rejection(e.kind);
            gw.live.record_rejected();
            return e.response(Envelope::OpenAi, "");
        }
    };

    let mut rec = CallRecorder::new(gw.clone(), trace);
    rec.timeline = crate::relay::Timeline::starting_at(started);
    rec.event.organisation_id = caller.key.organisation_id.clone();
    rec.event.workspace_id = caller.key.workspace_id.clone();
    rec.event.key_id = caller.key.id.clone();
    rec.event.mode = "managed".into();
    rec.event.interface = "openai".into();
    rec.event.path = "/v1/chat/completions".into();
    // Managed mode always asks for usage. A provider that reports none is estimated.
    rec.usage_policy = UsagePolicy::Estimate;

    match run(&gw, &caller, &parts.headers, body, &mut rec).await {
        Ok(Answer::Stream { mut head, incoming, transform, idle, target }) => {
            common::stamp(head.headers_mut(), &gw, &caller.snapshot, Some(&target), &rec);
            rec.timeline.client_head();
            let (head, ()) = head.into_parts();
            Response::from_parts(head, Body::new(RelayBody::new(incoming, rec, idle).with_transform(transform)))
        }
        Ok(Answer::Whole { mut head, body, target, cost }) => {
            let headers = head.headers_mut();
            common::stamp(headers, &gw, &caller.snapshot, Some(&target), &rec);
            if let Some(c) = cost
                && let Some(amount) = c.amount
                && let Ok(v) = HeaderValue::from_str(&format!("{amount:.6} {}", c.currency))
            {
                headers.insert("x-gw-cost", v);
            }
            headers.insert(CONTENT_LENGTH, HeaderValue::from(body.len()));
            rec.complete_from_http();
            rec.timeline.client_head();
            rec.timeline.client_chunk(Instant::now());
            rec.finish();
            let (head, ()) = head.into_parts();
            Response::from_parts(head, Body::from(body))
        }
        Err(e) => common::fail(&e, Envelope::OpenAi, rec),
    }
}

enum Answer {
    Stream {
        head: Response<()>,
        incoming: hyper::body::Incoming,
        transform: Box<dyn Transform>,
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

/// What the request shaping decided beyond the patched body.
struct Shaped {
    body: Bytes,
    stream: bool,
    /// The caller asked for the usage chunk on the stream.
    wants_usage: bool,
}

/// Removes `cache_control` markers from a list of messages or tools. Returns whether any was there.
fn strip_cache_markers(items: &mut [Value]) -> bool {
    let mut found = false;
    for item in items {
        let Some(obj) = item.as_object_mut() else { continue };
        found |= obj.remove("cache_control").is_some();
        for nested in ["content", "function"] {
            match obj.get_mut(nested) {
                Some(Value::Array(blocks)) => {
                    for b in blocks {
                        if let Some(o) = b.as_object_mut() {
                            found |= o.remove("cache_control").is_some();
                        }
                    }
                }
                Some(Value::Object(o)) => found |= o.remove("cache_control").is_some(),
                _ => {}
            }
        }
    }
    found
}

fn role_of(m: &Value) -> &str {
    m.get("role").and_then(Value::as_str).unwrap_or_default()
}

/// Whether two neighbouring turns can be joined into one.
fn mergeable(a: &Value, b: &Value) -> bool {
    let plain = |m: &Value| m.get("tool_calls").is_none() && m.get("tool_call_id").is_none();
    let role = role_of(a);
    matches!(role, "user" | "assistant" | "system") && role == role_of(b) && plain(a) && plain(b)
}

fn parts_of(content: Value) -> Vec<Value> {
    match content {
        Value::Array(parts) => parts,
        Value::String(s) if s.is_empty() => Vec::new(),
        Value::String(s) => vec![json!({ "type": "text", "text": s })],
        Value::Null => Vec::new(),
        other => vec![other],
    }
}

/// Joins `next` onto `into`. Two texts stay a text; anything else becomes a list of parts.
fn merge_turn(into: &mut Map<String, Value>, next: Map<String, Value>) {
    let a = into.remove("content").unwrap_or(Value::Null);
    let b = next.get("content").cloned().unwrap_or(Value::Null);
    let merged = match (a, b) {
        (Value::String(x), Value::String(y)) => Value::String(match (x.is_empty(), y.is_empty()) {
            (true, _) => y,
            (_, true) => x,
            _ => format!("{x}\n\n{y}"),
        }),
        (x, y) => {
            let mut parts = parts_of(x);
            parts.extend(parts_of(y));
            Value::Array(parts)
        }
    };
    into.insert("content".into(), merged);
}

/// Makes a history acceptable to a provider that rejects consecutive turns of one role and a
/// leading assistant turn (Gemini). Returns whether anything changed.
fn normalise_history(messages: &mut Vec<Value>) -> bool {
    let before = messages.len();
    // A leading assistant turn, after any system messages, has no user turn to answer.
    let first = messages.iter().position(|m| role_of(m) != "system");
    if let Some(i) = first {
        let lead =
            messages[i..].iter().take_while(|m| role_of(m) == "assistant" && m.get("tool_calls").is_none()).count();
        messages.drain(i..i + lead);
    }
    let mut out: Vec<Value> = Vec::with_capacity(messages.len());
    for m in messages.drain(..) {
        match (out.last_mut(), m) {
            (Some(prev), Value::Object(next)) if mergeable(prev, &Value::Object(next.clone())) => {
                if let Some(p) = prev.as_object_mut() {
                    merge_turn(p, next);
                }
            }
            (_, m) => out.push(m),
        }
    }
    *messages = out;
    messages.len() != before
}

/// Rewrites the request for the provider. Everything not named here keeps its bytes.
#[allow(clippy::too_many_arguments)]
fn shape_request(
    bytes: &Bytes,
    top: &TopLevel,
    target: &Target,
    params: &GatewayParams,
    policy: UnsupportedParams,
    rec: &mut CallRecorder,
) -> Result<Shaped, GwError> {
    let model = &target.model.doc;
    let caps = &model.capabilities;
    let quirks: &Quirks = &target.provider.doc.quirks;
    let spec = target.template.as_ref().map(|t| &t.spec);
    let mut dropped: Vec<String> = Vec::new();
    // What the model cannot take is dropped and reported, or the request is refused.
    let unsupported = |name: &str, what: &str, dropped: &mut Vec<String>| -> Result<(), GwError> {
        if policy == UnsupportedParams::Reject {
            return Err(GwError::unsupported_feature(format!("the model \"{}\" does not take {what}", model.name)));
        }
        dropped.push(name.to_owned());
        Ok(())
    };

    let mut patch = Patch::new();
    patch.set("model", &json!(model.upstream_id));
    patch.remove("gateway");

    // --- parameters ---
    let mut sent = shape::read(Some(Dialect::OpenAiChat), bytes, top);
    if let Some((raw, thinking)) = shape::inner(bytes, top, "thinking") {
        // An Anthropic-style thinking object is accepted on every model and mapped.
        match thinking.str(raw, "type").as_deref() {
            Some("enabled") => sent.params.reasoning_budget = thinking.u64(raw, "budget_tokens"),
            Some("disabled") if sent.params.reasoning_effort.is_none() => {
                sent.params.reasoning_effort = Some("none".into())
            }
            _ => {}
        }
        if !quirks.thinking_object {
            patch.remove("thinking");
        }
    }
    let cx = MergeContext {
        spec,
        capabilities: caps,
        terms: target.terms,
        model_max_output: model.max_output_tokens,
        policy,
        filter_caller: true,
    };
    let decisions = merge(&cx, &sent.params)?;
    let mut notes = shape::Notes::default();
    let shape_target = shape::Target { capabilities: caps, quirks, managed: true, cache_auto: false };
    shape::write(Some(Dialect::OpenAiChat), &mut patch, &decisions, &sent, &shape_target, &mut notes);
    dropped.extend(decisions.dropped.iter().cloned());
    dropped.extend(notes.dropped);

    // --- features the capability profile rules out ---
    let has_tools = top.get("tools").is_some_and(|e| e.kind == Kind::Array && e.value.len() > 2);
    if has_tools && !caps.tools {
        return Err(GwError::unsupported_feature(format!("the model \"{}\" does not take tools", model.name)));
    }
    if let Some(e) = top.get("tool_choice") {
        let raw = &bytes[e.value.clone()];
        let forced = e.kind == Kind::Object || raw == b"\"required\"";
        if forced && !caps.tool_choice_forced {
            unsupported("tool_choice", "a forced tool choice", &mut dropped)?;
            patch.remove("tool_choice");
        }
    }
    if let Some((raw, format)) = shape::inner(bytes, top, "response_format")
        && format.str(raw, "type").as_deref() == Some("json_schema")
        && !caps.structured_output
    {
        unsupported("response_format", "a JSON schema for its answer", &mut dropped)?;
        patch.remove("response_format");
    }
    for field in &quirks.drop_fields {
        if top.get(field).is_some() {
            unsupported(field, &format!("the field {field}"), &mut dropped)?;
            patch.remove(field.as_str());
        }
    }

    // --- content: touched only where the provider demands it ---
    let cache_mode = params.prompt_cache.or(spec.map(|s| s.prompt_cache)).unwrap_or_default();
    let strip = cache_mode == PromptCacheMode::Off || !quirks.cache_control;
    for member in ["messages", "tools"] {
        let Some(raw) = top.raw(bytes, member) else { continue };
        let has_markers = strip && memmem::find(raw, b"\"cache_control\"").is_some();
        let normalise = member == "messages" && quirks.normalise_history;
        if !has_markers && !normalise {
            continue;
        }
        let Ok(mut items) = serde_json::from_slice::<Vec<Value>>(raw) else { continue };
        let mut changed = false;
        if has_markers && strip_cache_markers(&mut items) {
            changed = true;
            // Markers the template switches off are the template's doing, not a shortfall.
            if cache_mode != PromptCacheMode::Off && !dropped.iter().any(|d| d == "cache_control") {
                unsupported("cache_control", "prompt-cache markers", &mut dropped)?;
            }
        }
        if normalise {
            changed |= normalise_history(&mut items);
        }
        if changed {
            patch.push(member, Op::Set(render(&Value::Array(items))));
        }
    }

    // --- streaming and usage ---
    let stream = top.bool(bytes, "stream").unwrap_or(false);
    let wants_usage =
        shape::inner(bytes, top, "stream_options").and_then(|(raw, o)| o.bool(raw, "include_usage")).unwrap_or(false);
    if stream {
        if quirks.stream_usage {
            patch.nested("stream_options", "include_usage", Op::Set(Bytes::from_static(b"true")));
        } else {
            patch.remove("stream_options");
        }
    }

    rec.event.dropped_params = dropped;
    rec.event.stream = stream;
    let patched = apply(bytes, top, &patch)
        .map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?;
    Ok(Shaped { body: patched.to_bytes(), stream, wants_usage })
}

/// The `cost` object the gateway adds to `usage` (concept section 9.3).
fn cost_object(c: &Costed) -> Value {
    match c.amount {
        Some(amount) if c.price_version > 0 => {
            json!({ "amount": amount, "currency": c.currency, "price_version": c.price_version })
        }
        Some(amount) => json!({ "amount": amount, "currency": c.currency }),
        None => json!({ "amount": null, "price_missing": true }),
    }
}

/// Adds the cost to the `usage` object of a response or stream chunk, and the cached-token
/// detail where the provider reported it under another name.
fn add_cost(data: &Bytes, top: &TopLevel, usage: &Usage, cost: Option<&Costed>) -> Option<Bytes> {
    let mut patch = Patch::new();
    if let Some(c) = cost {
        patch.nested("usage", "cost", Op::Set(render(&cost_object(c))));
    }
    if usage.cache_read_tokens > 0 {
        patch.nested(
            "usage",
            "prompt_tokens_details",
            Op::SetIfAbsent(render(&json!({ "cached_tokens": usage.cache_read_tokens }))),
        );
    }
    let out = apply(data, top, &patch).ok()?;
    out.changed().then(|| out.to_bytes())
}

async fn run(
    gw: &Arc<Gw>,
    caller: &Caller,
    headers: &HeaderMap,
    body: Body,
    rec: &mut CallRecorder,
) -> Result<Answer, GwError> {
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

    // --- request ---
    let limit = gw.boot.max_body_bytes;
    rec.timeline.body_begin();
    let bytes = match http_body_util::Limited::new(body, limit).collect().await {
        Ok(c) => c.to_bytes(),
        Err(_) => return Err(GwError::too_large(limit)),
    };
    rec.request_bytes = bytes.len() as u64;
    rec.timeline.body_done();
    let top =
        scan(&bytes).map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?;

    let mut params = GatewayParams::from_headers(headers)?;
    if let Some(e) = top.get("gateway") {
        params.merge_object(&bytes[e.value.clone()])?;
    }
    // OpenAI's own field for the end user is used when the gateway's is not given.
    if params.end_user.is_none() {
        params.end_user = top.str(&bytes, "user").filter(|u| !u.is_empty());
    }
    params.attribute(caller, rec);

    let model = top.str(&bytes, "model").unwrap_or_default();
    if model.is_empty() && params.template.is_none() {
        return Err(GwError::invalid_request("model is required: a model id or @template"));
    }
    if top.get("messages").is_none_or(|e| e.kind != Kind::Array) {
        return Err(GwError::invalid_request("messages is required and must be a list"));
    }
    rec.event.model_requested =
        if model.is_empty() { format!("@{}", params.template.as_deref().unwrap_or_default()) } else { model.clone() };

    // --- target ---
    let snap = &caller.snapshot;
    let target = resolve(
        snap,
        &caller.workspace,
        &caller.key,
        &ResolveRequest {
            model: &model,
            template: params.template.as_deref(),
            provider: None,
            families: Some(SAME_DIALECT),
        },
    )?;
    common::record_target(snap, &target, rec);
    if target.model.doc.kind != ModelKind::Chat {
        return Err(GwError::invalid_request(format!("the model \"{}\" is not a chat model", target.model.doc.name)));
    }
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

    // --- parameters, then admission ---
    let policy = params
        .unsupported_params
        .or(target.template.as_ref().and_then(|t| t.spec.unsupported_params))
        .unwrap_or(caller.workspace.settings.unsupported_params);
    let shaped = shape_request(&bytes, &top, &target, &params, policy, rec)?;
    common::admit(gw, caller, rec)?;

    // --- provider call ---
    let secret = credential.as_ref().map(|(s, _)| s.value.as_str());
    let (timeout, max_attempts) =
        common::timeout_and_attempts(Some(&target), provider.doc.default_timeout_seconds, &params);
    let plan = SendPlan {
        method: Method::POST,
        url: provider.doc.managed_url("chat/completions"),
        headers: common::managed_headers(headers, &[], scheme, secret, shaped.body.len(), shaped.stream)?,
        timeout,
        max_attempts,
        managed: true,
    };
    let sending = Instant::now();
    let sent = common::send(gw, &plan, Body::from(shaped.body.clone()), Some(&shaped.body), rec).await?;
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
    // The body the client gets is not the provider's byte for byte, so its length is not either.
    head.headers_mut().remove(CONTENT_LENGTH);
    let deadline = sending + timeout;

    // --- a stream is shaped event by event as it passes ---
    if parts.status.is_success() && content_type.starts_with("text/event-stream") {
        rec.event.stream = true;
        rec.tap = Some(UsageTap::new(Dialect::OpenAiChat, &content_type));
        let transform = Box::new(ChatStream { framer: SseFramer::new(), wants_usage: shaped.wants_usage });
        let idle = Duration::from_secs(gw.boot.stream_idle_seconds);
        return Ok(Answer::Stream { head, incoming, transform, idle, target });
    }

    // --- everything else is held, so that it can be accounted before it is answered ---
    rec.timeline.set_buffered();
    let cap = if parts.status.is_success() { MAX_HELD_RESPONSE } else { MAX_ERROR_BODY };
    let held = tokio::time::timeout_at(deadline.into(), http_body_util::Limited::new(incoming, cap).collect()).await;
    rec.timeline.upstream_chunk(Instant::now());
    let body = match held {
        Err(_) => return Err(GwError::upstream_timeout(format!("the provider did not finish within {timeout:?}"))),
        Ok(Err(e)) => return Err(GwError::upstream_error(format!("the provider's answer could not be read: {e}"))),
        Ok(Ok(c)) => c.to_bytes(),
    };
    rec.response_bytes = body.len() as u64;

    if !parts.status.is_success() {
        let said = provider_message(&body);
        let status = parts.status.as_u16();
        // The provider's limit and the provider's failures are the gateway's to report; what
        // the provider says about the request itself is passed on as it is.
        if status == 429 {
            let after =
                parts.headers.get(RETRY_AFTER).and_then(|v| v.to_str().ok()).and_then(|v| v.trim().parse::<f64>().ok());
            return Err(GwError::upstream_rate_limited(
                format!("the provider's rate limit was reached: {said}"),
                after.map(|s| s.ceil().clamp(1.0, 3600.0) as u32),
            ));
        }
        if parts.status.is_server_error() {
            return Err(GwError::upstream_error(format!("the provider answered {status}: {said}")));
        }
        head.headers_mut().insert("x-gw-error-type", HeaderValue::from_static("upstream_error"));
        return Ok(Answer::Whole { head, body, target, cost: None });
    }

    let found = parse_whole(Dialect::OpenAiChat, &body);
    let usage = found.has_usage.then_some(found.usage);
    rec.absorb(found);
    let cost = usage.as_ref().and_then(|u| rec.cost_of(u));
    let body = match (usage, scan(&body)) {
        (Some(u), Ok(top)) => add_cost(&body, &top, &u, cost.as_ref()).unwrap_or(body),
        _ => body,
    };
    Ok(Answer::Whole { head, body, target, cost })
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

/// Shapes an OpenAI-dialect stream on its way to the caller. Events pass as the bytes the
/// provider sent; only the chunk that carries usage is touched: the cost is added to it, or it
/// is left out when the caller did not ask for usage (the gateway always asks the provider).
struct ChatStream {
    framer: SseFramer,
    wants_usage: bool,
}

impl ChatStream {
    fn rewrite(&self, data: &Bytes, rec: &CallRecorder) -> Rewritten {
        // Every chunk of a stream with usage switched on carries `"usage":null`; only the
        // final one has an object there.
        if memmem::find(data, b"\"usage\"").is_none() || memmem::find(data, b"\"usage\":null").is_some() {
            return Rewritten::Same;
        }
        let Ok(top) = scan(data) else { return Rewritten::Same };
        if top.get("usage").is_none_or(|e| e.kind != Kind::Object) {
            return Rewritten::Same;
        }
        if !self.wants_usage {
            let no_choices =
                top.raw(data, "choices").is_none_or(|c| c.iter().filter(|b| !b.is_ascii_whitespace()).eq(b"[]"));
            if no_choices {
                return Rewritten::Dropped;
            }
            let mut patch = Patch::new();
            patch.remove("usage");
            return apply(data, &top, &patch).map_or(Rewritten::Same, |p| Rewritten::Data(p.to_bytes()));
        }
        let Some(usage) = openai_chat_usage(data) else { return Rewritten::Same };
        let cost = rec.cost_of(&usage);
        add_cost(data, &top, &usage, cost.as_ref()).map_or(Rewritten::Same, Rewritten::Data)
    }
}

enum Rewritten {
    Same,
    Dropped,
    Data(Bytes),
}

impl Transform for ChatStream {
    fn chunk(&mut self, chunk: &Bytes, rec: &mut CallRecorder) -> Bytes {
        let mut pieces: Vec<Bytes> = Vec::with_capacity(2);
        let mut framer = std::mem::take(&mut self.framer);
        framer.feed_raw(chunk, |raw, event| match event.as_ref().map(|ev| (ev, self.rewrite(&ev.data, rec))) {
            Some((_, Rewritten::Dropped)) => {}
            Some((ev, Rewritten::Data(data))) => {
                let mut out = BytesMut::with_capacity(data.len() + 16);
                write_event(&mut out, &ev.event, &data);
                pieces.push(out.freeze());
            }
            _ => pieces.push(raw),
        });
        self.framer = framer;
        match pieces.len() {
            0 => Bytes::new(),
            1 => pieces.pop().expect("one piece"),
            _ => {
                let mut out = BytesMut::with_capacity(pieces.iter().map(Bytes::len).sum());
                for p in &pieces {
                    out.extend_from_slice(p);
                }
                out.freeze()
            }
        }
    }

    fn end(&mut self, _rec: &mut CallRecorder) -> Bytes {
        // Whatever the provider sent after its last complete event goes out as it came.
        self.framer.take_pending()
    }

    fn error(&mut self, kind: &str, message: &str, rec: &mut CallRecorder) -> Bytes {
        let e = if kind == "upstream_timeout" {
            GwError::upstream_timeout(message)
        } else {
            GwError::upstream_error(format!("the provider's stream broke: {message}"))
        };
        let body = serde_json::to_vec(&e.body(Envelope::OpenAi, &rec.request_id())).unwrap_or_default();
        let mut out = BytesMut::new();
        write_event(&mut out, "", &body);
        out.freeze()
    }
}

// --- the model list ----------------------------------------------------------------------------

/// A model or template as the list endpoints describe it.
struct Listed {
    id: String,
    display_name: String,
    owned_by: String,
    /// `model` or `template`.
    kind: &'static str,
    /// For a template: the model it uses.
    uses: Option<String>,
    deprecated: bool,
}

/// What a key can put into `model`: the models granted to its workspace and callable, and
/// the workspace's templates (concept section 6.5).
fn listed(caller: &Caller) -> Vec<Listed> {
    let snap = &caller.snapshot;
    let key = &caller.key.doc;
    let mut out: Vec<Listed> = callable_models(snap, &caller.workspace, &caller.key)
        .into_iter()
        .map(|m| Listed {
            id: m.doc.name.clone(),
            display_name: if m.doc.display_name.is_empty() { m.doc.name.clone() } else { m.doc.display_name.clone() },
            owned_by: snap.providers.get(&m.doc.provider_id).map(|p| p.doc.name.clone()).unwrap_or_default(),
            kind: "model",
            uses: None,
            deprecated: m.doc.state == crate::model::ModelState::Deprecated,
        })
        .collect();
    let mut templates: Vec<_> = caller
        .workspace
        .templates
        .values()
        .filter(|t| key.allowed_templates.is_empty() || key.allowed_templates.iter().any(|a| *a == t.name))
        .collect();
    templates.sort_by(|a, b| a.name.cmp(&b.name));
    out.extend(templates.into_iter().map(|t| Listed {
        id: format!("@{}", t.name),
        display_name: format!("@{}", t.name),
        owned_by: "template".into(),
        kind: "template",
        uses: snap.models.get(&t.model_id).map(|m| m.doc.name.clone()),
        deprecated: false,
    }));
    out
}

fn anthropic_shape(headers: &HeaderMap) -> bool {
    headers.contains_key("anthropic-version")
}

fn entry(l: &Listed, anthropic: bool) -> Value {
    let gateway = json!({ "kind": l.kind, "model": l.uses, "deprecated": l.deprecated });
    if anthropic {
        json!({ "type": "model", "id": l.id, "display_name": l.display_name, "created_at": "1970-01-01T00:00:00Z", "gateway": gateway })
    } else {
        json!({ "id": l.id, "object": "model", "created": 0, "owned_by": l.owned_by, "gateway": gateway })
    }
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    let mut r = Response::new(Body::from(serde_json::to_vec(body).unwrap_or_default()));
    *r.status_mut() = status;
    r.headers_mut().insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    r
}

/// `GET /v1/models`: in Anthropic's shape when the request carries `anthropic-version`,
/// otherwise in OpenAI's.
pub async fn list_models(
    State(gw): State<Arc<Gw>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let anthropic = anthropic_shape(&headers);
    let envelope = if anthropic { Envelope::Anthropic } else { Envelope::OpenAi };
    let caller = match identify(&gw, &headers, Some(peer.ip())) {
        Ok(c) => c,
        Err(e) => return e.response(envelope, ""),
    };
    let all = listed(&caller);
    let data: Vec<Value> = all.iter().map(|l| entry(l, anthropic)).collect();
    let body = if anthropic {
        json!({ "data": data, "has_more": false, "first_id": all.first().map(|l| &l.id), "last_id": all.last().map(|l| &l.id) })
    } else {
        json!({ "object": "list", "data": data })
    };
    json_response(StatusCode::OK, &body)
}

/// `GET /v1/models/{id}`. Model ids contain slashes, so the id is the rest of the path.
pub async fn get_model(
    State(gw): State<Arc<Gw>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Response {
    let anthropic = anthropic_shape(&headers);
    let envelope = if anthropic { Envelope::Anthropic } else { Envelope::OpenAi };
    let caller = match identify(&gw, &headers, Some(peer.ip())) {
        Ok(c) => c,
        Err(e) => return e.response(envelope, ""),
    };
    match listed(&caller).iter().find(|l| l.id == id) {
        Some(l) => json_response(StatusCode::OK, &entry(l, anthropic)),
        None => GwError::not_found(
            "model_not_found",
            format!("the model \"{id}\" does not exist or is not available to this workspace"),
        )
        .response(envelope, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_markers_are_removed_wherever_callers_put_them() {
        let mut messages: Vec<Value> = serde_json::from_value(json!([
            { "role": "system", "content": [{ "type": "text", "text": "static", "cache_control": { "type": "ephemeral" } }] },
            { "role": "user", "content": "plain", "cache_control": { "type": "ephemeral" } },
            { "role": "assistant", "content": "no marker" }
        ]))
        .unwrap();
        assert!(strip_cache_markers(&mut messages));
        assert_eq!(
            Value::Array(messages.clone()),
            json!([
                { "role": "system", "content": [{ "type": "text", "text": "static" }] },
                { "role": "user", "content": "plain" },
                { "role": "assistant", "content": "no marker" }
            ])
        );
        assert!(!strip_cache_markers(&mut messages), "nothing left to remove");

        let mut tools: Vec<Value> = serde_json::from_value(
            json!([{ "type": "function", "function": { "name": "f", "cache_control": {} }, "cache_control": {} }]),
        )
        .unwrap();
        assert!(strip_cache_markers(&mut tools));
        assert_eq!(Value::Array(tools), json!([{ "type": "function", "function": { "name": "f" } }]));
    }

    #[test]
    fn a_history_is_made_acceptable_to_a_provider_that_wants_alternating_turns() {
        let mut m: Vec<Value> = serde_json::from_value(json!([
            { "role": "system", "content": "be brief" },
            { "role": "assistant", "content": "leftover greeting" },
            { "role": "user", "content": "first" },
            { "role": "user", "content": [{ "type": "image_url", "image_url": { "url": "data:x" } }] },
            { "role": "assistant", "content": "a" },
            { "role": "assistant", "content": "b" },
            { "role": "assistant", "content": null, "tool_calls": [{ "id": "c1" }] },
            { "role": "tool", "tool_call_id": "c1", "content": "r1" },
            { "role": "tool", "tool_call_id": "c2", "content": "r2" },
            { "role": "user", "content": "last" }
        ]))
        .unwrap();
        assert!(normalise_history(&mut m));
        assert_eq!(
            Value::Array(m.clone()),
            json!([
                { "role": "system", "content": "be brief" },
                { "role": "user", "content": [{ "type": "text", "text": "first" }, { "type": "image_url", "image_url": { "url": "data:x" } }] },
                { "role": "assistant", "content": "a\n\nb" },
                { "role": "assistant", "content": null, "tool_calls": [{ "id": "c1" }] },
                { "role": "tool", "tool_call_id": "c1", "content": "r1" },
                { "role": "tool", "tool_call_id": "c2", "content": "r2" },
                { "role": "user", "content": "last" }
            ])
        );
        // An acceptable history is left alone.
        assert!(!normalise_history(&mut m));
    }

    #[test]
    fn the_cost_is_added_inside_usage_and_nothing_else_moves() {
        let body = Bytes::from_static(
            br#"{"id":"c1","choices":[{"message":{"content":"hi  there"}}],"usage":{"prompt_tokens":10,"completion_tokens":5}}"#,
        );
        let top = scan(&body).unwrap();
        let usage = openai_chat_usage(&body).unwrap();
        let cost = Costed {
            amount: Some(0.007044),
            currency: "USD".into(),
            price_version: 37,
            variant: String::new(),
            price_missing: false,
        };
        let out = add_cost(&body, &top, &usage, Some(&cost)).unwrap();
        assert_eq!(
            std::str::from_utf8(&out).unwrap(),
            r#"{"id":"c1","choices":[{"message":{"content":"hi  there"}}],"usage":{"prompt_tokens":10,"completion_tokens":5,"cost":{"amount":0.007044,"currency":"USD","price_version":37}}}"#
        );
        // A model without a price says so instead of showing a zero.
        assert_eq!(cost_object(&Costed::unpriced()), json!({ "amount": null, "price_missing": true }));
        assert_eq!(cost_object(&Costed::free("USD")), json!({ "amount": 0.0, "currency": "USD" }));

        // DeepSeek reports cache hits under its own name: the standard detail is added.
        let ds = Bytes::from_static(
            br#"{"usage":{"prompt_tokens":100,"completion_tokens":1,"prompt_cache_hit_tokens":60}}"#,
        );
        let u = openai_chat_usage(&ds).unwrap();
        let out = add_cost(&ds, &scan(&ds).unwrap(), &u, None).unwrap();
        assert!(
            std::str::from_utf8(&out)
                .unwrap()
                .ends_with(r#""prompt_cache_hit_tokens":60,"prompt_tokens_details":{"cached_tokens":60}}}"#)
        );
    }

    #[test]
    fn provider_error_messages_are_found_in_the_usual_places() {
        assert_eq!(
            provider_message(br#"{"error":{"message":"context too long","type":"invalid_request_error"}}"#),
            "context too long"
        );
        assert_eq!(provider_message(br#"{"error":"overloaded"}"#), "overloaded");
        assert_eq!(provider_message(b"upstream connect error"), "upstream connect error");
        assert_eq!(provider_message(&[b'x'; 1000]).len(), 300);
    }
}
