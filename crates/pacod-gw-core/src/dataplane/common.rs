//! What proxy mode and managed mode share: gateway parameters, attribution, admission,
//! header handling and sending to the provider with the reliability rules of concept
//! section 4.4.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;
use std::time::Duration;

use axum::body::Body;
use axum::http::header::{ACCEPT, ACCEPT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, HOST, RETRY_AFTER, USER_AGENT};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method};
use axum::response::Response;
use bytes::Bytes;
use chrono::Utc;
use pacod_gw_events::usage::status;
use serde::Deserialize;

use super::Gw;
use super::auth::Caller;
use super::errors::{Envelope, GwError};
use crate::limits::Subjects;
use crate::model::{AuditGuarantee, AuthScheme, PromptCacheMode, UnsupportedParams};
use crate::relay::{CallRecorder, ConcurrencySlot, Pricing, audit};
use crate::resolve::Target;
use crate::snapshot::{CredentialOwner, ModelRt, Snapshot};
use crate::telemetry::metrics;
use crate::upstream::{SendError, Sent};

/// Headers that belong to one connection and are never forwarded.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Request headers that carry the caller's credential or describe the hop to the gateway.
const NOT_FORWARDED: &[&str] =
    &["authorization", "x-api-key", "x-goog-api-key", "host", "content-length", "forwarded", "via"];

pub fn is_hop_by_hop(name: &str) -> bool {
    HOP_BY_HOP.contains(&name)
}

// --- gateway parameters ------------------------------------------------------------------------

/// The parameters that concern the gateway and not the model (concept section 9.2). They come
/// from the `x-gw-*` headers and, in managed mode, from the `gateway` object of the body.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GatewayParams {
    pub template: Option<String>,
    pub end_user: Option<String>,
    pub session_id: Option<String>,
    pub trace_id: Option<String>,
    pub tags: BTreeMap<String, String>,
    pub agent_id: Option<String>,
    pub user_id: Option<String>,
    pub task: Option<String>,
    pub complexity: Option<Complexity>,
    pub timeout_ms: Option<u64>,
    pub retries: Option<u8>,
    pub unsupported_params: Option<UnsupportedParams>,
    pub audit: Option<bool>,
    pub prompt_cache: Option<PromptCacheMode>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct GatewayObject {
    template: Option<String>,
    end_user: Option<String>,
    session_id: Option<String>,
    trace_id: Option<String>,
    tags: BTreeMap<String, serde_json::Value>,
    agent_id: Option<String>,
    user_id: Option<String>,
    task: Option<String>,
    complexity: Option<Complexity>,
    timeout_ms: Option<u64>,
    retry: Option<RetryObject>,
    unsupported_params: Option<UnsupportedParams>,
    audit: Option<bool>,
    cache: Option<CacheObject>,
}

pub use crate::model::Complexity;

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct RetryObject {
    max_attempts: Option<u8>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CacheObject {
    prompt: Option<PromptCacheMode>,
}

/// Attribution values are identifiers, not content: they are cut to a sane length.
fn clip(s: &str, max: usize) -> String {
    s.trim().chars().take(max).collect()
}

fn bad(name: &str, expected: &str) -> GwError {
    GwError::invalid_request(format!("{name} must be {expected}"))
}

impl GatewayParams {
    /// Reads the `x-gw-*` request headers.
    pub fn from_headers(headers: &HeaderMap) -> Result<Self, GwError> {
        let get = |name: &str| headers.get(name).and_then(|v| v.to_str().ok()).map(str::trim).filter(|v| !v.is_empty());
        let mut p = GatewayParams {
            template: get("x-gw-template").map(|v| v.trim_start_matches('@').to_owned()),
            end_user: get("x-gw-end-user").map(|v| clip(v, 200)),
            session_id: get("x-gw-session").map(|v| clip(v, 200)),
            trace_id: get("x-gw-trace").map(|v| clip(v, 200)),
            agent_id: get("x-gw-agent-id").map(|v| clip(v, 200)),
            user_id: get("x-gw-user-id").map(|v| clip(v, 200)),
            task: get("x-gw-task").map(|v| clip(v, 64)),
            ..Default::default()
        };
        if let Some(v) = get("x-gw-tags") {
            for pair in v.split(',') {
                if let Some((k, val)) = pair.split_once('=') {
                    p.tags.insert(k.trim().to_owned(), clip(val, 64));
                }
            }
        }
        if let Some(v) = get("x-gw-complexity") {
            p.complexity = Some(match v {
                "low" => Complexity::Low,
                "medium" => Complexity::Medium,
                "high" => Complexity::High,
                _ => return Err(bad("x-gw-complexity", "low, medium or high")),
            });
        }
        if let Some(v) = get("x-gw-timeout-ms") {
            p.timeout_ms = Some(v.parse().map_err(|_| bad("x-gw-timeout-ms", "a number of milliseconds"))?);
        }
        if let Some(v) = get("x-gw-retries") {
            p.retries = Some(v.parse().map_err(|_| bad("x-gw-retries", "a number of attempts"))?);
        }
        if let Some(v) = get("x-gw-unsupported-params") {
            p.unsupported_params = Some(match v {
                "drop" => UnsupportedParams::Drop,
                "reject" => UnsupportedParams::Reject,
                _ => return Err(bad("x-gw-unsupported-params", "drop or reject")),
            });
        }
        if let Some(v) = get("x-gw-audit") {
            p.audit = Some(matches!(v, "1" | "true" | "on"));
        }
        if let Some(v) = get("x-gw-prompt-cache") {
            p.prompt_cache = Some(match v {
                "off" => PromptCacheMode::Off,
                "passthrough" => PromptCacheMode::Passthrough,
                "auto" => PromptCacheMode::Auto,
                _ => return Err(bad("x-gw-prompt-cache", "off, passthrough or auto")),
            });
        }
        Ok(p)
    }

    /// Adds the `gateway` object of a request body. Where both name a value, the body wins:
    /// it is the more specific place.
    pub fn merge_object(&mut self, raw: &[u8]) -> Result<(), GwError> {
        let o: GatewayObject = serde_json::from_slice(raw)
            .map_err(|e| GwError::invalid_request(format!("the gateway object is not valid: {e}")))?;
        if let Some(v) = o.template {
            self.template = Some(v.trim_start_matches('@').to_owned());
        }
        if let Some(v) = o.end_user {
            self.end_user = Some(clip(&v, 200));
        }
        if let Some(v) = o.session_id {
            self.session_id = Some(clip(&v, 200));
        }
        if let Some(v) = o.trace_id {
            self.trace_id = Some(clip(&v, 200));
        }
        for (k, v) in o.tags {
            let v = match v {
                serde_json::Value::String(s) => s,
                serde_json::Value::Null => continue,
                other => other.to_string(),
            };
            self.tags.insert(k, clip(&v, 64));
        }
        if let Some(v) = o.agent_id {
            self.agent_id = Some(clip(&v, 200));
        }
        if let Some(v) = o.user_id {
            self.user_id = Some(clip(&v, 200));
        }
        if let Some(v) = o.task {
            self.task = Some(clip(&v, 64));
        }
        self.complexity = o.complexity.or(self.complexity);
        self.timeout_ms = o.timeout_ms.or(self.timeout_ms);
        self.retries = o.retry.and_then(|r| r.max_attempts).or(self.retries);
        self.unsupported_params = o.unsupported_params.or(self.unsupported_params);
        self.audit = o.audit.or(self.audit);
        self.prompt_cache = o.cache.and_then(|c| c.prompt).or(self.prompt_cache);
        Ok(())
    }

    /// Writes the attribution onto the event. Tag keys the workspace has not declared are
    /// dropped and returned, so that the caller can be told.
    pub fn attribute(&self, caller: &Caller, rec: &mut CallRecorder) -> Vec<String> {
        if let Some(v) = &self.end_user {
            rec.event.end_user = v.clone();
        }
        if let Some(v) = &self.session_id {
            rec.event.session_id = v.clone();
        }
        if let Some(v) = &self.trace_id {
            rec.event.trace_id = v.clone();
        }
        if let Some(v) = &self.agent_id {
            rec.event.agent_id = v.clone();
        }
        if let Some(v) = &self.user_id {
            rec.event.user_id = v.clone();
        }
        if let Some(v) = &self.task {
            rec.event.task = v.clone();
        }
        if let Some(v) = self.complexity {
            rec.event.complexity = v.name().to_owned();
        }
        let mut undeclared = Vec::new();
        for (k, v) in &self.tags {
            if caller.workspace.doc.tag_keys.iter().any(|d| d == k) {
                rec.event.tags.insert(k.clone(), v.clone());
            } else {
                undeclared.push(k.clone());
            }
        }
        undeclared
    }
}

// --- target, price and admission ---------------------------------------------------------------

/// The price that applies to a call on `model` now.
pub fn pricing_for(snap: &Snapshot, model: &ModelRt) -> Pricing {
    match snap.price_at(&model.id, Utc::now()) {
        Some(p) => Pricing::Priced(p.clone()),
        None if model.doc.free => Pricing::Free(snap.settings.reporting_currency.clone()),
        None => Pricing::Missing,
    }
}

/// Writes what resolution decided onto the event: the model that serves the call, the
/// template and its version, the template's tags and the price.
pub fn record_target(snap: &Snapshot, target: &Target, rec: &mut CallRecorder) {
    rec.event.model_served = target.model.doc.name.clone();
    rec.event.provider = target.provider.doc.name.clone();
    rec.pricing = pricing_for(snap, &target.model);
    if let Some(t) = &target.template {
        rec.event.template = t.name.clone();
        rec.event.template_version = t.version;
        // A template's tags describe the purpose of the call; they are not the caller's to change.
        for (k, v) in &t.spec.tags {
            rec.event.tags.insert(k.clone(), v.clone());
        }
    }
}

/// The credential a target is called with, for the audit record: its id and whose it is.
pub fn credential_json(caller: &Caller, target: &Target) -> serde_json::Value {
    target.provider.credential(&caller.key.workspace_id, &caller.key.organisation_id).map_or(
        serde_json::Value::Null,
        |(s, owner)| serde_json::json!({ "id": s.id, "own": owner == CredentialOwner::Own }),
    )
}

/// Writes what routing decided onto the event, and the `x-gw-route` header.
pub fn record_route(target: &Target, route: Option<&crate::resolve::RouteChoice>, rec: &mut CallRecorder) {
    let (Some(t), Some(route)) = (target.template.as_ref(), route) else { return };
    let action = route.action(t);
    rec.event.routing_source = t.route.source.name().to_owned();
    if let Some(s) = &t.route.scheme {
        rec.event.routing_scheme = s.name.to_string();
        rec.event.routing_scheme_version = u32::try_from(s.version).unwrap_or_default();
        rec.event.routing_rule = action.rule.as_deref().unwrap_or("default").to_owned();
    }
    rec.route_header = action.header.clone();
}

/// Admission (concept section 12.2): the key's concurrency cap, then every limit that
/// applies. On success the limits travel with the recorder and are settled when the call ends.
pub fn admit(gw: &Gw, caller: &Caller, rec: &mut CallRecorder) -> Result<(), GwError> {
    admit_slot(gw, caller, rec)?;
    admit_limits(gw, caller, rec).map_err(|(e, _)| e)
}

/// The key's concurrency cap. Once per call, whichever models it tries.
pub fn admit_slot(gw: &Gw, caller: &Caller, rec: &mut CallRecorder) -> Result<(), GwError> {
    if let Some(max) = caller.key.doc.max_concurrency {
        let counter: Arc<AtomicU32> = gw.key_slots.lock().entry(caller.key.id.clone()).or_default().clone();
        let slot = ConcurrencySlot::take(&counter, max)
            .ok_or_else(|| GwError::rate_limited("this key has too many calls in progress", 1))?;
        rec.slot = Some(slot);
    }
    Ok(())
}

/// The limits that apply to the call on the model it is about to try (`model_served`). A call
/// that moves on to another model keeps the limits both models share, counted once, and
/// trades the rest. A refusal says whether the limit counts only certain models, so that
/// another model may still serve.
pub fn admit_limits(gw: &Gw, caller: &Caller, rec: &mut CallRecorder) -> Result<(), (GwError, bool)> {
    let subjects = Subjects {
        workspace: &caller.workspace,
        key: &caller.key,
        end_user: &rec.event.end_user,
        model: &rec.event.model_served,
        template: &rec.event.template,
        tags: &rec.event.tags,
    };
    let applicable = gw.limits.applicable(&subjects, Utc::now());
    if applicable.is_empty() && rec.limits.is_empty() {
        return Ok(());
    }
    gw.limits.readmit(&mut rec.limits, applicable).map_err(|r| (GwError::from_refusal(&r), r.model_specific))?;
    if let Some(a) = rec.audit.as_mut() {
        // The limits the call was checked against, as they stood when it was admitted.
        a.resolution["limits"] = serde_json::json!(gw.limits.state(&rec.limits));
    }
    Ok(())
}

/// The value of `x-gw-quota-remaining`: what is left of each limit the call counts against,
/// as `name=remaining` pairs. Spend limits are in the reporting currency.
pub fn quota_header(gw: &Gw, rec: &CallRecorder) -> Option<HeaderValue> {
    if rec.limits.is_empty() {
        return None;
    }
    let pairs: Vec<String> = gw
        .limits
        .state(&rec.limits)
        .iter()
        .take(8)
        .map(|s| {
            // Whole numbers for tokens and requests; spend keeps its fraction.
            if s.remaining.fract() == 0.0 {
                format!("{}={}", s.name, s.remaining as u64)
            } else {
                format!("{}={:.6}", s.name, s.remaining)
            }
        })
        .collect();
    HeaderValue::from_str(&pairs.join(", ")).ok()
}

/// The time allowed for the provider and the number of attempts, from the template and the
/// caller. The caller can lower both and raise neither.
pub fn timeout_and_attempts(
    target: Option<&Target>,
    provider_default_seconds: u32,
    params: &GatewayParams,
) -> (Duration, u8) {
    let spec = target.and_then(|t| t.template.as_ref()).map(|t| &t.spec);
    let ceiling = spec.and_then(|s| s.timeout_seconds).unwrap_or(provider_default_seconds).clamp(1, 600);
    let mut timeout = Duration::from_secs(u64::from(ceiling));
    if let Some(ms) = params.timeout_ms {
        timeout = timeout.min(Duration::from_millis(ms.max(1)));
    }
    let max = spec.and_then(|s| s.retry_max_attempts).unwrap_or(2).clamp(1, 5);
    let attempts = params.retries.map_or(max, |r| r.clamp(1, max));
    (timeout, attempts)
}

// --- audit mode --------------------------------------------------------------------------------

/// A number in `[0, 1)`, for sampling.
fn draw() -> f64 {
    (u64::from_le_bytes(crate::crypto::random_bytes::<8>()) >> 11) as f64 / (1u64 << 53) as f64
}

/// Starts the audit record of a call when audit mode applies to it (concept section 14.6).
/// With `guarantee: required` the call is refused when the record could not be stored.
#[allow(clippy::too_many_arguments)]
pub fn begin_audit(
    gw: &Gw,
    caller: &Caller,
    target: Option<&Target>,
    params: &GatewayParams,
    method: &str,
    path_and_query: &str,
    headers: &HeaderMap,
    body: Option<&Bytes>,
    route: &str,
    warnings: &[String],
    rec: &mut CallRecorder,
) -> Result<(), GwError> {
    let template = target.and_then(|t| t.template.as_deref());
    let Some(plan) = audit::plan(&caller.workspace, template, params.audit, Utc::now(), draw()) else { return Ok(()) };
    if plan.guarantee == AuditGuarantee::Required && !gw.audit_spool.can_take() {
        return Err(GwError::unavailable(
            "audit_unavailable",
            "this call must be recorded in full, and the record cannot be stored at the moment",
        ));
    }
    let mut capture =
        audit::AuditCapture::begin(plan, rec.timeline.start, method, path_and_query, headers, &caller.key.id, body);
    let e = &rec.event;
    let credential = target.map(|t| credential_json(caller, t));
    capture.resolution = serde_json::json!({
        "key_id": caller.key.id,
        "workspace": caller.workspace.doc.name,
        "organisation_id": caller.key.organisation_id,
        "end_user": e.end_user,
        "session_id": e.session_id,
        "trace_id": e.trace_id,
        "tags": e.tags,
        "agent_id": e.agent_id,
        "user_id": e.user_id,
        "task": e.task,
        "complexity": e.complexity,
        "route": route,
        "template": target.and_then(|t| t.template.as_ref()).map(|t| serde_json::json!({ "name": t.name, "version": t.version })),
        "model_requested": e.model_requested,
        "model_served": e.model_served,
        "replaced_model": target.and_then(|t| t.replaced.as_ref()).map(|m| m.doc.name.clone()),
        "provider": e.provider,
        "credential": credential,
        "dropped_params": e.dropped_params,
        "warnings": warnings,
        "limits": gw.limits.state(&rec.limits),
    });
    rec.audit = Some(Box::new(capture));
    Ok(())
}

// --- headers -----------------------------------------------------------------------------------

fn credential_header(scheme: AuthScheme, secret: &str) -> Result<Option<(HeaderName, HeaderValue)>, GwError> {
    let (name, value) = match scheme {
        AuthScheme::Bearer => ("authorization", format!("Bearer {secret}")),
        AuthScheme::XApiKey => ("x-api-key", secret.to_owned()),
        AuthScheme::XGoogApiKey => ("x-goog-api-key", secret.to_owned()),
        AuthScheme::None => return Ok(None),
    };
    let mut value = HeaderValue::from_str(&value)
        .map_err(|_| GwError::internal("the provider credential contains characters a header cannot carry"))?;
    value.set_sensitive(true);
    Ok(Some((HeaderName::from_static(name), value)))
}

/// Proxy mode: the caller's headers with the credential replaced and everything that
/// concerns only the hop to the gateway removed.
pub fn relayed_headers(
    incoming: &HeaderMap,
    scheme: AuthScheme,
    credential: Option<&str>,
    body_len: Option<usize>,
) -> Result<HeaderMap, GwError> {
    let mut out = HeaderMap::with_capacity(incoming.len() + 3);
    for (name, value) in incoming {
        let n = name.as_str();
        if is_hop_by_hop(n) || NOT_FORWARDED.contains(&n) || n.starts_with("x-gw-") || n.starts_with("x-forwarded-") {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    // Compressed responses could not be read by the usage tap.
    out.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    if let Some(len) = body_len {
        out.insert(CONTENT_LENGTH, HeaderValue::from(len));
    }
    if let Some(secret) = credential
        && let Some((name, value)) = credential_header(scheme, secret)?
    {
        out.insert(name, value);
    }
    Ok(out)
}

/// Managed mode: the gateway builds the provider request itself, so only the headers named
/// in `forward` are taken from the caller.
pub fn managed_headers(
    incoming: &HeaderMap,
    forward: &[&'static str],
    scheme: AuthScheme,
    credential: Option<&str>,
    body_len: usize,
    stream: bool,
) -> Result<HeaderMap, GwError> {
    let mut out = HeaderMap::with_capacity(8);
    out.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    out.insert(ACCEPT, HeaderValue::from_static(if stream { "text/event-stream" } else { "application/json" }));
    out.insert(ACCEPT_ENCODING, HeaderValue::from_static("identity"));
    out.insert(USER_AGENT, HeaderValue::from_static(concat!("pacod-gw/", env!("CARGO_PKG_VERSION"))));
    out.insert(CONTENT_LENGTH, HeaderValue::from(body_len));
    for name in forward {
        for value in incoming.get_all(*name) {
            out.append(HeaderName::from_static(name), value.clone());
        }
    }
    if let Some(secret) = credential
        && let Some((name, value)) = credential_header(scheme, secret)?
    {
        out.insert(name, value);
    }
    Ok(out)
}

/// Builds the headers for the response to the client from the provider's.
pub fn client_headers(upstream: &HeaderMap, owner: CredentialOwner) -> HeaderMap {
    let mut out = HeaderMap::with_capacity(upstream.len() + 6);
    for (name, value) in upstream {
        let n = name.as_str();
        if is_hop_by_hop(n) {
            continue;
        }
        // Rate-limit headers describe the provider key. With a shared key they would tell one
        // workspace about the traffic of others.
        if n.contains("ratelimit") && owner == CredentialOwner::Shared {
            continue;
        }
        out.append(name.clone(), value.clone());
    }
    out
}

pub fn upstream_request_id(headers: &HeaderMap) -> Option<&str> {
    ["request-id", "x-request-id"].iter().find_map(|n| headers.get(*n)).and_then(|v| v.to_str().ok())
}

/// The gateway's own response headers that are known when the response head is written.
pub fn stamp(headers: &mut HeaderMap, gw: &Gw, snap: &Snapshot, target: Option<&Target>, rec: &CallRecorder) {
    let mut put = |name: &'static str, value: &str| {
        if let Ok(v) = HeaderValue::from_str(value) {
            headers.insert(name, v);
        }
    };
    put("x-gw-request-id", &rec.request_id());
    put("x-gw-attempts", &rec.event.attempts.to_string());
    put("x-gw-delay-ms", &format!("{:.3}", rec.timeline.delay_at_head_ms()));
    if !rec.event.model_served.is_empty() {
        put("x-gw-model-served", &rec.event.model_served);
    }
    if !rec.event.upstream_request_id.is_empty() {
        put("x-gw-upstream-request-id", &rec.event.upstream_request_id);
    }
    if !rec.event.dropped_params.is_empty() {
        put("x-gw-dropped-params", &rec.event.dropped_params.join(","));
    }
    if let Some(d) = target.and_then(|t| t.deprecation_header(snap)) {
        put("x-gw-model-deprecated", &d);
    }
    route_headers(headers, rec);
    if let Some(q) = quota_header(gw, rec) {
        headers.insert("x-gw-quota-remaining", q);
    }
}

/// `x-gw-route` (the scheme and rule that chose the model) and `x-gw-failovers`.
fn route_headers(headers: &mut HeaderMap, rec: &CallRecorder) {
    if let Some(v) = &rec.route_header {
        headers.insert("x-gw-route", v.clone());
    }
    if rec.event.failovers > 0 {
        headers.insert("x-gw-failovers", HeaderValue::from(u16::from(rec.event.failovers)));
    }
}

/// Ends a call that did not produce a provider response, and builds the error response in
/// the caller's dialect. The call is settled here.
pub fn fail(e: &GwError, envelope: Envelope, mut rec: CallRecorder) -> Response {
    let outcome = if e.kind == "upstream_timeout" {
        status::TIMEOUT
    } else if e.source == "upstream" {
        status::ERROR
    } else {
        status::REJECTED
    };
    if e.source == "gateway" {
        metrics::record_rejection(e.kind);
    }
    rec.outcome(outcome, e.status.as_u16(), e.kind, &e.message);
    let mut resp = e.response(envelope, &rec.request_id());
    let h = resp.headers_mut();
    if let Ok(v) = HeaderValue::from_str(&format!("{:.3}", rec.timeline.delay_at_head_ms())) {
        h.insert("x-gw-delay-ms", v);
    }
    h.insert("x-gw-attempts", HeaderValue::from(u16::from(rec.event.attempts)));
    route_headers(h, &rec);
    let request_id = rec.request_id();
    if let Some(a) = rec.audit.as_mut() {
        // The gateway's own answer is what the client was sent.
        a.client_head(e.status.as_u16(), resp.headers(), false);
        a.client_chunk(&serde_json::to_vec(&e.body(envelope, &request_id)).unwrap_or_default());
    }
    rec.timeline.client_head();
    rec.finish();
    resp
}

// --- sending -----------------------------------------------------------------------------------

/// How a request is sent to the provider.
pub struct SendPlan {
    pub method: Method,
    pub url: String,
    pub headers: HeaderMap,
    /// For a stream: until the response head. Otherwise the caller also bounds the body.
    pub timeout: Duration,
    pub max_attempts: u8,
    /// Managed mode: a `429` or `5xx` answer and a broken connection are retried while
    /// attempts remain. Proxy mode relays the provider's answer and retries only when nothing
    /// can have reached the provider.
    pub managed: bool,
}

/// The longest the gateway waits between attempts. A provider that asks for more gets its
/// answer passed on instead.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(8);

fn backoff(attempt: u8) -> Duration {
    // 200 ms, 400 ms, 800 ms … with up to 25 % jitter, so that callers do not retry in step.
    let base = 200u64 << (attempt.saturating_sub(1)).min(5);
    let jitter = u64::from(crate::crypto::random_bytes::<1>()[0]) * base / 1024;
    Duration::from_millis(base + jitter).min(MAX_RETRY_WAIT)
}

fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let v = headers.get(RETRY_AFTER)?.to_str().ok()?;
    v.trim().parse::<f64>().ok().filter(|s| s.is_finite() && *s >= 0.0).map(Duration::from_secs_f64)
}

/// Sends the request, following the reliability rules (concept section 4.4): a timeout is
/// never retried, and nothing is retried once a response is passed on. `replay` is the body in
/// memory; without it (a blind relay) there is one attempt only.
pub async fn send(
    gw: &Gw,
    plan: &SendPlan,
    first: Body,
    replay: Option<&Bytes>,
    rec: &mut CallRecorder,
) -> Result<Sent, GwError> {
    let build = |body: Body| -> Result<http::Request<Body>, GwError> {
        let mut r = http::Request::builder()
            .method(plan.method.clone())
            .uri(&plan.url)
            .body(body)
            .map_err(GwError::internal)?;
        *r.headers_mut() = plan.headers.clone();
        if let Some(host) = r.uri().authority().map(|a| a.as_str().to_owned())
            && let Ok(v) = HeaderValue::from_str(&host)
        {
            r.headers_mut().insert(HOST, v);
        }
        Ok(r)
    };

    let mut next = Some(first);
    // Counted per send: a call that moves on to another model starts afresh there.
    let mut attempt: u8 = 0;
    loop {
        rec.timeline.attempt();
        rec.event.attempts = rec.event.attempts.saturating_add(1);
        attempt = attempt.saturating_add(1);
        let may_retry = attempt < plan.max_attempts && replay.is_some();
        let body = match next.take() {
            Some(b) => b,
            None => Body::from(replay.cloned().unwrap_or_default()),
        };
        if let Some(a) = rec.audit.as_mut() {
            a.attempt(plan.method.as_str(), &plan.url, &plan.headers, replay);
        }
        match gw.upstream.send(build(body)?, plan.timeout).await {
            Ok(sent) => {
                let status = sent.response.status();
                if let Some(a) = rec.audit.as_mut() {
                    a.attempt_head(status.as_u16(), sent.response.headers());
                }
                if plan.managed && may_retry && (status.as_u16() == 429 || status.is_server_error()) {
                    let wait = retry_after(sent.response.headers()).unwrap_or_else(|| backoff(attempt));
                    if wait <= MAX_RETRY_WAIT {
                        // Retries are counted from the event's attempts when the call settles.
                        drop(sent);
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                }
                return Ok(sent);
            }
            Err(SendError::Timeout(d)) => {
                return Err(GwError::upstream_timeout(format!("the provider did not answer within {d:?}")));
            }
            Err(SendError::Egress(m)) => return Err(GwError::not_allowed(m)),
            Err(e) if may_retry && (e.nothing_sent() || plan.managed) => {
                if plan.managed {
                    tokio::time::sleep(backoff(attempt)).await;
                }
            }
            Err(e) => return Err(GwError::upstream_error(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hm(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.append(*k, HeaderValue::from_static(v));
        }
        h
    }

    #[test]
    fn gateway_parameters_from_headers_and_body() {
        let mut p = GatewayParams::from_headers(&hm(&[
            ("x-gw-template", "@planner"),
            ("x-gw-end-user", " u_1 "),
            ("x-gw-tags", "purpose=plan, site=pacod,broken"),
            ("x-gw-timeout-ms", "1500"),
            ("x-gw-retries", "1"),
            ("x-gw-unsupported-params", "reject"),
            ("x-gw-prompt-cache", "auto"),
            ("x-gw-audit", "true"),
            ("x-gw-agent-id", "planner"),
            ("x-gw-user-id", "staff-1"),
            ("x-gw-task", "summarise"),
            ("x-gw-complexity", "low"),
        ]))
        .unwrap();
        assert_eq!(p.agent_id.as_deref(), Some("planner"));
        assert_eq!(p.user_id.as_deref(), Some("staff-1"));
        assert_eq!(p.task.as_deref(), Some("summarise"));
        assert_eq!(p.complexity, Some(Complexity::Low));
        assert_eq!(p.template.as_deref(), Some("planner"));
        assert_eq!(p.end_user.as_deref(), Some("u_1"));
        assert_eq!(p.tags.len(), 2);
        assert_eq!(p.tags["site"], "pacod");
        assert_eq!((p.timeout_ms, p.retries), (Some(1500), Some(1)));
        assert_eq!(p.unsupported_params, Some(UnsupportedParams::Reject));
        assert_eq!(p.prompt_cache, Some(PromptCacheMode::Auto));
        assert_eq!(p.audit, Some(true));

        // The body object wins where both are given, and adds to the tags.
        p.merge_object(
            br#"{"end_user":"u_2","tags":{"purpose":"synth","n":3},"retry":{"max_attempts":2},
            "cache":{"prompt":"off"},"fallback":["x"],"idempotency_key":"k",
            "agent_id":"writer","complexity":"high"}"#,
        )
        .unwrap();
        assert_eq!(p.end_user.as_deref(), Some("u_2"));
        assert_eq!(p.tags["purpose"], "synth");
        assert_eq!(p.tags["n"], "3");
        assert_eq!(p.retries, Some(2));
        assert_eq!(p.prompt_cache, Some(PromptCacheMode::Off));
        assert_eq!(p.timeout_ms, Some(1500), "what the body does not name is kept");
        assert_eq!(p.agent_id.as_deref(), Some("writer"));
        assert_eq!(p.complexity, Some(Complexity::High));
        assert_eq!(p.task.as_deref(), Some("summarise"));

        assert!(GatewayParams::from_headers(&hm(&[("x-gw-timeout-ms", "soon")])).is_err());
        assert!(GatewayParams::from_headers(&hm(&[("x-gw-unsupported-params", "maybe")])).is_err());
        assert!(GatewayParams::default().merge_object(br#"{"timeout_ms":"x"}"#).is_err());
        assert!(GatewayParams::from_headers(&hm(&[("x-gw-complexity", "extreme")])).is_err());
        assert!(GatewayParams::default().merge_object(br#"{"complexity":"extreme"}"#).is_err());

        let long = "t".repeat(100);
        let mut p = GatewayParams::default();
        p.merge_object(format!(r#"{{"task":"{long}"}}"#).as_bytes()).unwrap();
        assert_eq!(p.task.map(|t| t.len()), Some(64), "a task is a label and is cut like a tag value");
    }

    #[test]
    fn the_caller_can_lower_timeout_and_attempts_but_not_raise_them() {
        let none = GatewayParams::default();
        assert_eq!(timeout_and_attempts(None, 180, &none), (Duration::from_secs(180), 2));
        let lower = GatewayParams { timeout_ms: Some(2_000), retries: Some(1), ..Default::default() };
        assert_eq!(timeout_and_attempts(None, 180, &lower), (Duration::from_secs(2), 1));
        let higher = GatewayParams { timeout_ms: Some(900_000), retries: Some(9), ..Default::default() };
        assert_eq!(timeout_and_attempts(None, 180, &higher), (Duration::from_secs(180), 2));
    }

    #[test]
    fn the_callers_credential_is_replaced_and_gateway_headers_are_dropped() {
        let incoming = hm(&[
            ("authorization", "Bearer pgw_secret"),
            ("x-api-key", "pgw_secret"),
            ("anthropic-version", "2023-06-01"),
            ("anthropic-beta", "a"),
            ("anthropic-beta", "b"),
            ("x-gw-end-user", "u1"),
            ("x-forwarded-for", "10.0.0.1"),
            ("connection", "keep-alive"),
            ("accept-encoding", "gzip, br"),
            ("content-length", "999"),
            ("host", "gateway.example"),
            ("content-type", "application/json"),
        ]);
        let out = relayed_headers(&incoming, AuthScheme::XApiKey, Some("sk-ant-real"), Some(42)).unwrap();
        assert_eq!(out["x-api-key"], "sk-ant-real");
        assert!(out["x-api-key"].is_sensitive());
        assert!(out.get("authorization").is_none(), "the gateway key never reaches the provider");
        assert_eq!(out.get_all("anthropic-beta").iter().count(), 2, "repeated headers are kept");
        assert_eq!(out["anthropic-version"], "2023-06-01");
        assert!(out.get("x-gw-end-user").is_none() && out.get("x-forwarded-for").is_none());
        assert!(out.get("connection").is_none() && out.get("host").is_none());
        assert_eq!(out["accept-encoding"], "identity");
        assert_eq!(out["content-length"], "42");
        assert_eq!(out["content-type"], "application/json");

        let out = relayed_headers(&incoming, AuthScheme::Bearer, Some("sk-real"), None).unwrap();
        assert_eq!(out["authorization"], "Bearer sk-real");
        assert!(out.get("x-api-key").is_none());
        assert!(out.get("content-length").is_none());

        let out = relayed_headers(&incoming, AuthScheme::None, None, Some(1)).unwrap();
        assert!(out.get("authorization").is_none() && out.get("x-api-key").is_none());
    }

    #[test]
    fn managed_requests_carry_only_what_the_gateway_chose() {
        let incoming = hm(&[
            ("authorization", "Bearer pgw_secret"),
            ("openai-organization", "org_caller"),
            ("anthropic-beta", "a"),
            ("cookie", "session=1"),
        ]);
        let out =
            managed_headers(&incoming, &["anthropic-beta"], AuthScheme::Bearer, Some("sk-real"), 10, true).unwrap();
        assert_eq!(out["authorization"], "Bearer sk-real");
        assert_eq!(out["accept"], "text/event-stream");
        assert_eq!(out["anthropic-beta"], "a");
        assert!(out.get("openai-organization").is_none() && out.get("cookie").is_none());
        assert_eq!(out["content-length"], "10");
    }

    #[test]
    fn rate_limit_headers_are_only_passed_for_an_own_credential() {
        let upstream = hm(&[
            ("anthropic-ratelimit-requests-remaining", "49"),
            ("x-ratelimit-remaining-tokens", "1000"),
            ("retry-after", "3"),
            ("request-id", "req_1"),
            ("transfer-encoding", "chunked"),
            ("content-type", "text/event-stream"),
        ]);
        let shared = client_headers(&upstream, CredentialOwner::Shared);
        assert!(shared.get("anthropic-ratelimit-requests-remaining").is_none());
        assert!(shared.get("x-ratelimit-remaining-tokens").is_none());
        assert_eq!(shared["retry-after"], "3");
        assert_eq!(shared["request-id"], "req_1");
        assert!(shared.get("transfer-encoding").is_none());
        let own = client_headers(&upstream, CredentialOwner::Own);
        assert_eq!(own["anthropic-ratelimit-requests-remaining"], "49");
        assert_eq!(upstream_request_id(&upstream), Some("req_1"));
        assert_eq!(retry_after(&upstream), Some(Duration::from_secs(3)));
    }

    #[test]
    fn backoff_grows_and_stays_bounded() {
        assert!(backoff(1) >= Duration::from_millis(200) && backoff(1) < Duration::from_millis(260));
        assert!(backoff(3) >= Duration::from_millis(800));
        assert!(backoff(60) <= MAX_RETRY_WAIT);
    }
}
