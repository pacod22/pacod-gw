//! Proxy mode (concept section 4.2): the provider's own API, relayed byte for byte.
//!
//! The gateway authenticates the caller, checks what it is allowed to do, replaces the
//! credential, and relays. The request body is read but not rewritten, and a read-only tap on
//! the response extracts usage. Without a template nothing in either body changes; with one,
//! only top-level parameter fields of the request are written (section 7.4).

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::HeaderName;
use axum::http::header::{CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, Method, Uri};
use axum::response::Response;
use bytes::Bytes;
use http_body_util::BodyExt;
use serde_json::json;

use super::Gw;
use super::auth::{Caller, authenticate};
use super::client_ip::ClientIp;
use super::common::{self, GatewayParams, SendPlan};
use super::errors::{Envelope, GwError};
use super::managed::{Failure, route_input};
use super::{shape, tools};
use crate::extract::{Dialect, UsageTap, dialect_for};
use crate::json::{Op, Patch, TopLevel, apply, scan};
use crate::model::{
    AuthScheme, Family, InspectMode, Mode, PromptCacheMode, ProxyBehaviour, Status, UnsupportedParams, UsagePolicy,
};
use crate::params::{MergeContext, check, merge};
use crate::relay::{CallRecorder, RelayBody};
use crate::resolve::{
    ResolveRequest, Target, next_target, resolve_model, resolve_routed, resolve_upstream, template_for,
};
use crate::snapshot::{CredentialOwner, ProviderRt, TemplateRt};
use crate::telemetry::metrics;
use crate::telemetry::trace::TraceContext;

/// The error envelope the caller's SDK understands: the one of the API the path belongs to.
fn envelope_for(family: Option<Family>, path: &str) -> Envelope {
    match family {
        Some(Family::Openai) => Envelope::OpenAi,
        // Cohere's errors are `{message}`, which OpenAI's SDKs and Cohere's read alike.
        Some(Family::Cohere) => Envelope::OpenAi,
        Some(Family::Anthropic) => Envelope::Anthropic,
        // Gemini's OpenAI-compatible endpoint, and its native API.
        Some(Family::Gemini) if path.starts_with("/v1beta/openai/") => Envelope::OpenAi,
        Some(Family::Gemini) => Envelope::Google,
        // Ollama's native API, and its OpenAI-compatible one under /v1.
        Some(Family::Ollama) if path.starts_with("/api/") => Envelope::Ollama,
        Some(Family::Ollama) => Envelope::OpenAi,
        // TypeSafe's errors are `{message}`; no SDK of TypeSafe's reads the gateway's errors,
        // so they are the gateway's own, as on an unknown provider.
        Some(Family::Typesafe) | None => Envelope::Gateway,
    }
}

/// For Vertex AI, the caller's Google headers are dropped: `x-goog-user-project` would bill
/// another project, the method overrides would undo "POST only", and the others steer what the
/// token may reach. The gateway's own credential is added after this.
fn vertex_headers(mut headers: HeaderMap, vertex: bool) -> HeaderMap {
    if vertex {
        let dropped: Vec<HeaderName> = headers
            .keys()
            .filter(|n| {
                let n = n.as_str();
                n.starts_with("x-goog-")
                    || n == "x-http-method-override"
                    || n == "x-http-method"
                    || n == "x-method-override"
            })
            .cloned()
            .collect();
        for name in dropped {
            headers.remove(name);
        }
    }
    headers
}

/// What the gateway read from the request body.
#[derive(Debug, Default, PartialEq)]
struct Inspected {
    model: Option<String>,
    stream: bool,
    include_usage: bool,
}

fn inspect(body: &[u8], top: &TopLevel) -> Inspected {
    let include_usage = shape::inner(body, top, "stream_options")
        .and_then(|(raw, inner)| inner.bool(raw, "include_usage"))
        .unwrap_or(false);
    Inspected { model: top.str(body, "model"), stream: top.bool(body, "stream").unwrap_or(false), include_usage }
}

/// Gemini's native API names the model in the path: `/v1beta/models/{model}:generateContent`.
fn model_from_path(path: &str) -> Option<String> {
    let rest = path.split("/models/").nth(1)?;
    let model = rest.split([':', '/', '?']).next()?;
    (!model.is_empty()).then(|| model.to_owned())
}

/// A provider's model list, or one entry of it: it names no model the gateway could check and
/// reaches nothing that belongs to a workspace.
fn is_model_list(method: &Method, path: &str) -> bool {
    if method != Method::GET {
        return false;
    }
    let path = path.split('?').next().unwrap_or(path).trim_end_matches('/');
    let mut segments = path.rsplit('/');
    let (last, before) = (segments.next().unwrap_or_default(), segments.next().unwrap_or_default());
    last == "models" || before == "models" || path == "/api/tags"
}

fn is_json(headers: &HeaderMap) -> bool {
    headers
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().starts_with("application/json"))
}

pub async fn handle(
    State(gw): State<Arc<Gw>>,
    Path(params): Path<Vec<(String, String)>>,
    ClientIp(client): ClientIp,
    req: Request,
) -> Response {
    let started = Instant::now();
    let provider_name = params.first().map(|(_, v)| v.as_str()).unwrap_or_default().to_owned();
    let rest = params.get(1).map(|(_, v)| v.as_str()).unwrap_or_default();
    let path = format!("/{rest}");

    let (mut parts, body) = req.into_parts();
    let trace = TraceContext::from_traceparent(parts.headers.get("traceparent").and_then(|v| v.to_str().ok()));
    super::auth::take_query_key(&mut parts);

    // The provider decides the error envelope, so it is looked up first. An unknown provider
    // is only reported after authentication, so the path does not reveal which providers exist.
    let envelope = envelope_for(gw.config.snapshot().provider_by_name(&provider_name).map(|p| p.doc.family), &path);

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
    rec.usage_policy = caller.key.usage_policy(&caller.workspace);

    let params = match GatewayParams::from_headers(&parts.headers) {
        Ok(p) => p,
        Err(e) => return common::fail(&e, envelope, rec),
    };
    params.attribute(&caller, &mut rec);

    match relay(&gw, &caller, &provider_name, &parts.method, &parts.uri, &path, parts.headers, body, &params, &mut rec)
        .await
    {
        Ok(r) => {
            let (mut head, ()) = r.head.into_parts();
            common::stamp(&mut head.headers, &gw, &caller.snapshot, r.target.as_ref(), &rec);
            rec.timeline.client_head();
            Response::from_parts(head, Body::new(RelayBody::new(r.incoming, rec, r.idle).with_deadline(r.deadline)))
        }
        Err(e) => common::fail(&e, envelope, rec),
    }
}

struct Relayed {
    head: Response<()>,
    incoming: hyper::body::Incoming,
    idle: Duration,
    /// When a response that is not a stream must be complete.
    deadline: Option<Instant>,
    target: Option<Target>,
}

/// What a template changed in a request.
struct Applied {
    body: Option<Bytes>,
    /// The relayed path, when the model in it was replaced (Gemini's native API).
    path: Option<String>,
    include_usage: bool,
}

/// Applies or checks a template on a proxy request (concept section 7.4).
#[allow(clippy::too_many_arguments)]
fn template(
    target: &Target,
    tpl: &TemplateRt,
    provider: &ProviderRt,
    dialect: Option<Dialect>,
    path: &str,
    body: Option<(&Bytes, &TopLevel)>,
    inspected: &Inspected,
    rec: &mut CallRecorder,
) -> Result<Applied, GwError> {
    let upstream_id = target.model.doc.upstream_id.as_str();
    let in_path = model_from_path(path);
    // Gemini's two spellings of `generationConfig` become one, so that the template governs it.
    let one_spelling = match (dialect, body) {
        (Some(Dialect::Gemini), Some((b, top))) => shape::gemini_one_spelling(b, top)
            .map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?,
        _ => None,
    };
    let rescanned = one_spelling.as_ref().map(|b| scan(b)).transpose().map_err(GwError::internal)?;
    let body = one_spelling.as_ref().zip(rescanned.as_ref()).or(body);
    let sent = body.map(|(b, top)| shape::read(dialect, b, top)).unwrap_or_default();
    let cx = MergeContext {
        spec: Some(&tpl.spec),
        capabilities: &target.model.doc.capabilities,
        terms: target.terms,
        model_max_output: target.model.doc.max_output_tokens,
        // The caller's own parameters are not filtered in proxy mode.
        policy: UnsupportedParams::Drop,
        filter_caller: false,
    };
    let mut out = Applied { body: None, path: None, include_usage: inspected.include_usage };

    if tpl.spec.proxy == ProxyBehaviour::Check {
        // Nothing is written: the request must already be what the template describes.
        let named = inspected.model.as_deref().or(in_path.as_deref());
        if named != Some(upstream_id) {
            return Err(GwError::template_violation(format!(
                "the template \"@{}\" requires the model \"{upstream_id}\"",
                tpl.name
            )));
        }
        check(&cx, &sent.params)
            .map_err(|why| GwError::template_violation(format!("the template \"@{}\" is not met: {why}", tpl.name)))?;
        return Ok(out);
    }

    if let Some(m) = &in_path
        && m != upstream_id
    {
        out.path = Some(path.replacen(&format!("/models/{m}"), &format!("/models/{upstream_id}"), 1));
    }
    let Some((bytes, top)) = body else { return Ok(out) };

    let decisions = merge(&cx, &sent.params)?;
    let mut patch = Patch::new();
    // The model goes where this API names it: in the body, unless the path carries it.
    if top.get("model").is_some() || (in_path.is_none() && dialect.is_some()) {
        patch.set("model", &json!(upstream_id));
    }
    let mut notes = shape::Notes::default();
    let shape_target = shape::Target {
        capabilities: &target.model.doc.capabilities,
        quirks: &provider.doc.quirks,
        managed: false,
        cache_auto: tpl.spec.prompt_cache == PromptCacheMode::Auto && dialect == Some(Dialect::Anthropic),
    };
    shape::write(dialect, &mut patch, &decisions, &sent, &shape_target, &mut notes);
    if tpl.spec.request_stream_usage
        && dialect == Some(Dialect::OpenAiChat)
        && inspected.stream
        && !inspected.include_usage
    {
        patch.nested("stream_options", "include_usage", Op::Set(Bytes::from_static(b"true")));
        out.include_usage = true;
    }
    rec.param_sources = decisions.sources.clone();
    rec.event.dropped_params = decisions.dropped.into_iter().chain(notes.dropped).collect();

    let patched = apply(bytes, top, &patch)
        .map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?;
    if patched.changed() || one_spelling.is_some() {
        out.body = Some(patched.to_bytes());
    }
    Ok(out)
}

/// The request for a target: the template's model and parameters written in, when the
/// target comes from a template. Returns the path and the body to send.
#[allow(clippy::too_many_arguments)]
fn request_for(
    target: &Target,
    provider: &ProviderRt,
    dialect: Option<Dialect>,
    path: &str,
    buffered: Option<&Bytes>,
    scanned: Option<&TopLevel>,
    inspected: &mut Inspected,
    rec: &mut CallRecorder,
) -> Result<(String, Option<Bytes>), GwError> {
    let Some(tpl) = &target.template else { return Ok((path.to_owned(), buffered.cloned())) };
    let applied = template(target, tpl, provider, dialect, path, buffered.zip(scanned), inspected, rec)?;
    inspected.include_usage = applied.include_usage;
    Ok((applied.path.unwrap_or_else(|| path.to_owned()), applied.body.or_else(|| buffered.cloned())))
}

#[allow(clippy::too_many_arguments)]
async fn relay(
    gw: &Arc<Gw>,
    caller: &Caller,
    provider_name: &str,
    method: &Method,
    uri: &Uri,
    path: &str,
    headers: HeaderMap,
    body: Body,
    params: &GatewayParams,
    rec: &mut CallRecorder,
) -> Result<Relayed, GwError> {
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
    // Vertex AI: only the model methods of the provider's own project, as POST; that replaces
    // the paths of its family.
    match &provider.vertex {
        Some(site) => site
            .allows(method.as_str(), path, uri.query())
            .map_err(|why| GwError::not_allowed(format!("{why}: the path {path} is not relayed to Vertex AI")))?,
        None if !provider.doc.allows_proxy_path(path) => {
            return Err(GwError::not_allowed(format!("the path {path} is not relayed for this provider")));
        }
        None => {}
    }
    if provider.doc.family == Family::Anthropic {
        common::check_anthropic_betas(&headers, snap)?;
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

    let dialect = dialect_for(provider.doc.family, path);
    let blind = caller.key.doc.inspect == InspectMode::None;
    if dialect.is_none() {
        // A path without a usage extractor (a model list, a file) is counted as a request;
        // an estimate from its bytes would invent tokens.
        rec.usage_policy = UsagePolicy::Ignore;
    }

    // --- request body ---
    rec.timeline.body_begin();
    let (mut upstream_body, buffered): (Option<Body>, Option<Bytes>) = if blind {
        rec.request_bytes =
            headers.get(CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()).unwrap_or(0);
        (Some(body), None)
    } else {
        if headers.get(CONTENT_ENCODING).is_some_and(|v| v != "identity") {
            return Err(GwError::invalid_request(
                "compressed request bodies are not accepted; send the body uncompressed",
            ));
        }
        let limit = gw.boot.max_body_bytes;
        let bytes = match http_body_util::Limited::new(body, limit).collect().await {
            Ok(c) => c.to_bytes(),
            Err(_) => return Err(GwError::too_large(limit)),
        };
        rec.request_bytes = bytes.len() as u64;
        (None, Some(bytes))
    };
    rec.timeline.body_done();

    // --- inspection ---
    // A model call is read for its model; one the gateway cannot read would pass without the
    // model and grant checks.
    // A form (an image edit, a transcription) is relayed unread: it needs a credential of the
    // caller's own, as a call without a checked model does.
    if dialect.is_some()
        && !crate::extract::multipart_path(path)
        && buffered.as_ref().is_some_and(|b| !b.is_empty())
        && !is_json(&headers)
    {
        return Err(GwError::unsupported_media_type(format!(
            "the path {path} takes a JSON body: send it with the content type application/json"
        )));
    }
    let mut inspected = Inspected::default();
    let mut scanned: Option<TopLevel> = None;
    if let Some(bytes) = &buffered
        && !bytes.is_empty()
        && is_json(&headers)
    {
        let top = scan(bytes)
            .map_err(|e| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}")))?;
        inspected = inspect(bytes, &top);
        scanned = Some(top);
        // Embeddings a provider reports no usage for (Gemini's) are estimated from their input,
        // not from the size of the vectors that come back.
        if matches!(dialect, Some(crate::extract::Dialect::OpenAiEmbeddings | crate::extract::Dialect::GeminiEmbed))
            && let Some(chars) = crate::extract::embedding_input_chars(bytes)
        {
            rec.fallback_usage =
                Some(pacod_gw_events::Usage { input_tokens: chars.div_ceil(4) as u64, ..Default::default() });
        }
    }
    // Provider tools the workspace does not allow are refused; nothing in the body is changed.
    // A blind relay cannot be checked, so a workspace that restricts them has none.
    if let Some(allowed) = caller.workspace.settings.provider_tools.as_deref() {
        if blind {
            return Err(GwError::not_allowed(
                "this workspace restricts provider tools, which a key set to blind relay cannot check",
            ));
        }
        if let (Some(bytes), Some(top)) = (&buffered, &scanned) {
            tools::check(bytes, top, allowed)?;
        }
    }
    let named_model = inspected.model.clone().or_else(|| model_from_path(path));
    rec.event.model_requested = named_model.clone().unwrap_or_default();
    rec.event.stream = inspected.stream || path.contains(":streamGenerateContent");

    // --- plugins ---
    // A plugin that checks every call cannot check a body it is not given (docs/plugin-concept.md,
    // section 3.9). Paths without a conversation (model lists, files, embeddings) pass.
    if blind && caller.workspace.enforces_plugins() {
        return Err(GwError::not_allowed(
            "this workspace checks every call with a plugin, which a key set to blind relay cannot pass",
        ));
    }
    let signals = match (&buffered, dialect.and_then(crate::plugins::proxy_dialect)) {
        (Some(bytes), Some(d)) if !bytes.is_empty() => {
            let model =
                params.template.as_ref().map(|t| format!("@{t}")).or_else(|| named_model.clone()).unwrap_or_default();
            let stream = rec.event.stream;
            crate::plugins::run::before_plan(
                gw,
                caller,
                || {
                    Some(crate::plugins::input(
                        caller,
                        params,
                        "proxy",
                        Some(provider_name),
                        path,
                        model,
                        stream,
                        bytes,
                        d,
                    ))
                },
                rec,
            )
            .await?
        }
        _ => None,
    };

    // --- template and model ---
    let template_name =
        params.template.clone().or_else(|| named_model.as_deref().and_then(|m| m.strip_prefix('@')).map(str::to_owned));
    if blind && template_name.is_some() {
        return Err(GwError::invalid_request("a key set to blind relay cannot use templates"));
    }
    if caller.key.doc.require_template && template_name.is_none() {
        return Err(GwError::not_allowed("this key accepts proxy requests only when they name a template"));
    }
    let mut route = None;
    let target = match (&template_name, &named_model) {
        (Some(t), _) => {
            let req = ResolveRequest { model: "", template: Some(t), provider: Some(&provider), families: None };
            let tpl = template_for(&caller.workspace, &caller.key, t)?;
            // A checking template takes the request's own model when it is one of its models;
            // otherwise routing chooses among those at this provider.
            let own = (tpl.spec.proxy == ProxyBehaviour::Check)
                .then(|| {
                    tpl.pool.iter().position(|m| {
                        m.model.doc.provider_id == provider.id
                            && named_model.as_deref() == Some(m.model.doc.upstream_id.as_str())
                    })
                })
                .flatten();
            match own {
                Some(i) => Some(resolve_model(snap, &caller.workspace, &caller.key, &req, Some(&tpl), Some(i))?),
                None => {
                    let input = route_input(
                        params,
                        "proxy",
                        inspected.stream,
                        buffered.as_ref().map_or(0, Bytes::len),
                        signals.as_ref(),
                    );
                    let (target, choice) = resolve_routed(snap, &caller.workspace, &caller.key, &req, &input)?;
                    route = choice;
                    Some(target)
                }
            }
        }
        // A blind relay does not read the request, so there is no model to check.
        (None, Some(m)) if !blind => Some(resolve_upstream(snap, &caller.workspace, &caller.key, &provider, m)?),
        _ => None,
    };

    // A call without a model the gateway checked (a file, a batch, a blind relay) reaches
    // whatever the credential reaches at the provider. On the installation's shared credential
    // that includes other workspaces' files and batches, so it needs a credential of its own.
    if target.is_none() && owner == CredentialOwner::Shared && !is_model_list(method, path) {
        return Err(GwError::own_credential_required(if scheme == AuthScheme::None {
            format!("only model calls and model lists are relayed to the provider \"{provider_name}\"")
        } else {
            format!(
                "this call names no model the gateway can check, so it needs a credential of this workspace \
                 or organisation for the provider \"{provider_name}\""
            )
        }));
    }
    // Files a call to Anthropic names must be this workspace's (concept section 8.3).
    if provider.doc.family == Family::Anthropic
        && let Some(bytes) = &buffered
    {
        super::files::check(gw, caller, owner, bytes).await?;
    }
    // What OpenAI's Responses API keeps for the shared credential is every workspace's.
    if dialect == Some(Dialect::OpenAiResponses)
        && owner == CredentialOwner::Shared
        && let Some(bytes) = &buffered
    {
        super::responses::check_shared(bytes)?;
    }
    // Gemini's files and context caches are reached only on a credential of the caller's own;
    // a blind relay is not read, so on the shared credential it is not relayed to Gemini.
    if provider.doc.family == Family::Gemini && owner == CredentialOwner::Shared {
        match &buffered {
            Some(bytes) => super::files::check_gemini(owner, bytes, provider.vertex.is_some())?,
            None if !is_model_list(method, path) => {
                return Err(GwError::own_credential_required(format!(
                    "a blind relay to the provider \"{provider_name}\" needs a credential of this workspace or organisation"
                )));
            }
            None => {}
        }
    }

    let mut target = target;
    let (mut out_path, mut out_body) = (path.to_owned(), buffered.clone());
    if let Some(t) = &target {
        common::record_target(snap, t, rec);
        common::record_route(t, route.as_ref(), rec);
        (out_path, out_body) =
            request_for(t, &provider, dialect, path, buffered.as_ref(), scanned.as_ref(), &mut inspected, rec)?;
    }

    if !blind
        && dialect == Some(Dialect::OpenAiChat)
        && inspected.stream
        && !inspected.include_usage
        && rec.usage_policy == UsagePolicy::Require
    {
        return Err(GwError::usage_required());
    }

    // --- audit mode, then admission ---
    let query = uri.query().map(|q| format!("?{q}")).unwrap_or_default();
    common::begin_audit(
        gw,
        caller,
        target.as_ref(),
        params,
        method.as_str(),
        &format!("/proxy/{provider_name}{path}{query}"),
        &headers,
        buffered.as_ref(),
        "proxy",
        &[],
        rec,
    )?;
    common::admit_slot(gw, caller, rec)?;

    // --- provider request ---
    // With a template of several models at this provider, a failing model hands the call on to
    // the next one routing chose, before anything of an answer has reached the caller.
    let failover = match (&target, &route) {
        (Some(t), Some(r)) if !blind && r.candidates.len() > 1 => t.template.as_ref().map(|tpl| r.action(tpl).failover),
        _ => None,
    };
    // A service account's token opens a whole cloud project: it is relayed only to a provider
    // on Vertex AI, whose calls were pinned to its own project's models above.
    if scheme == AuthScheme::GoogleServiceAccount && provider.vertex.is_none() {
        return Err(GwError::not_allowed("a service account's token is relayed only to Vertex AI"));
    }
    let auth = common::gw_auth_header(gw, scheme, credential.as_ref().map(|(s, _)| s.as_ref())).await?;
    let (sent, sending, timeout) = loop {
        common::admit_limits(gw, caller, rec).map_err(|(e, _)| e)?;
        let (mut timeout, mut max_attempts) =
            common::timeout_and_attempts(target.as_ref(), provider.doc.default_timeout_seconds, params);
        if let Some(f) = failover {
            max_attempts = f.attempts_per_target.unwrap_or(1);
            if let Some(ms) = f.per_target_timeout_ms {
                timeout = timeout.min(Duration::from_millis(ms.into()));
            }
        }
        if let Some(cap) = params.retries {
            max_attempts = max_attempts.min(cap.saturating_sub(rec.event.attempts)).max(1);
        }
        let plan = SendPlan {
            method: method.clone(),
            url: format!("{}{out_path}{query}", provider.proxy_origin),
            // The caller's Google headers go before the credential is added: on Vertex AI's
            // express mode the credential is one of them (`x-goog-api-key`).
            headers: common::relayed_headers(
                &vertex_headers(headers.clone(), provider.vertex.is_some()),
                &auth,
                out_body.as_ref().map(Bytes::len),
            ),
            timeout,
            max_attempts,
            managed: false,
            error_rules: None,
        };
        let first = match upstream_body.take() {
            Some(b) => b,
            None => Body::from(out_body.clone().unwrap_or_default()),
        };
        let sending = Instant::now();
        let result = common::send(gw, &plan, first, out_body.as_ref(), rec).await;
        let failure = match &result {
            Ok(s) => Failure::of_answer(s.response.status(), s.response.headers()),
            Err(e) => Failure::of_error(e),
        };
        let next = match (failure, failover, &target, route.as_mut()) {
            (Some(f), Some(policy), Some(t), Some(r))
                if f.allowed_by(&policy) && params.retries.is_none_or(|cap| rec.event.attempts < cap) =>
            {
                t.template.clone().and_then(|tpl| {
                    let req = ResolveRequest {
                        model: "",
                        template: Some(&tpl.name),
                        provider: Some(&provider),
                        families: None,
                    };
                    next_target(snap, &caller.workspace, &caller.key, &req, &tpl, r).map(|n| (n, f))
                })
            }
            _ => None,
        };
        let Some((next, f)) = next else { break (result?, sending, timeout) };
        let from = target.as_ref().map(|t| t.model.doc.name.clone()).unwrap_or_default();
        metrics::record_failover(&provider.doc.name, &from, f.name());
        if f != Failure::LimitRefused {
            gw.live.record_health(&provider.doc.name, &from, false, 0.0);
        }
        rec.event.models_tried.push(from);
        rec.event.failovers = rec.event.failovers.saturating_add(1);
        rec.event.http_status = 0;
        common::record_target(snap, &next, rec);
        (out_path, out_body) =
            request_for(&next, &provider, dialect, path, buffered.as_ref(), scanned.as_ref(), &mut inspected, rec)?;
        target = Some(next);
    };
    rec.timeline.connected(sent.connected_at);
    rec.timeline.upstream_head(sent.head_at);

    // --- response head ---
    let (parts, incoming) = sent.response.into_parts();
    rec.event.http_status = parts.status.as_u16();
    if let Some(id) = common::upstream_request_id(&parts.headers) {
        rec.event.upstream_request_id = id.chars().take(128).collect();
    }
    let content_type = parts.headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default().to_owned();
    if content_type.starts_with("text/event-stream") {
        rec.event.stream = true;
    }
    if parts.status.is_success()
        && let Some(d) = dialect
        && parts.headers.get(CONTENT_ENCODING).is_none_or(|v| v == "identity")
    {
        rec.tap = Some(UsageTap::new(d, &content_type).with_fields(provider.usage_fields.clone()));
    }

    let mut head = Response::new(());
    *head.status_mut() = parts.status;
    *head.headers_mut() = common::client_headers(&parts.headers, owner);
    let stream = rec.event.stream;
    if let Some(a) = rec.audit.as_mut() {
        a.client_head(parts.status.as_u16(), head.headers(), stream);
    }
    // The time allowed covers the whole answer of a call that is not a stream. Paths the
    // gateway does not model, such as file downloads, are left to the idle timeout.
    let deadline = (!rec.event.stream && dialect.is_some()).then(|| sending + timeout);
    Ok(Relayed { head, incoming, idle: Duration::from_secs(gw.boot.stream_idle_seconds), deadline, target })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn top_of(body: &[u8]) -> Inspected {
        inspect(body, &scan(body).unwrap())
    }

    #[test]
    fn reads_model_stream_and_usage_option() {
        assert_eq!(
            top_of(br#"{"model":"gpt-x","stream":true,"stream_options":{"include_usage":true},"messages":[]}"#),
            Inspected { model: Some("gpt-x".into()), stream: true, include_usage: true }
        );
        assert_eq!(
            top_of(br#"{"messages":[{"model":"inner","stream":true}],"stream_options":{"include_usage":false}}"#),
            Inspected { model: None, stream: false, include_usage: false }
        );
        assert!(!top_of(br#"{"model":"m","stream_options":null}"#).include_usage);
    }

    #[test]
    fn gemini_model_comes_from_the_path() {
        assert_eq!(
            model_from_path("/v1beta/models/gemini-3.8-flash:generateContent").as_deref(),
            Some("gemini-3.8-flash")
        );
        assert_eq!(
            model_from_path("/v1beta/models/gemini-x:streamGenerateContent?alt=sse").as_deref(),
            Some("gemini-x")
        );
        assert_eq!(model_from_path("/v1beta/models/@planner:generateContent").as_deref(), Some("@planner"));
        assert_eq!(model_from_path("/v1/chat/completions"), None);
        assert_eq!(model_from_path("/v1beta/models/"), None);
    }

    #[test]
    fn model_lists_are_recognised() {
        for path in ["/v1/models", "/v1/models/", "/v1beta/models", "/v1/models/gpt-x", "/api/tags"] {
            assert!(is_model_list(&Method::GET, path), "{path}");
        }
        for path in ["/v1/files", "/v1/messages/batches", "/api/ps", "/v1/models/gpt-x/extra"] {
            assert!(!is_model_list(&Method::GET, path), "{path}");
        }
        assert!(!is_model_list(&Method::POST, "/v1/models"));
        assert!(!is_model_list(&Method::DELETE, "/v1/models/ft-x"));
    }

    #[test]
    fn envelope_follows_the_provider_family() {
        assert_eq!(envelope_for(Some(Family::Anthropic), "/v1/messages"), Envelope::Anthropic);
        assert_eq!(envelope_for(Some(Family::Openai), "/v1/chat/completions"), Envelope::OpenAi);
        assert_eq!(envelope_for(Some(Family::Gemini), "/v1beta/models/m:generateContent"), Envelope::Google);
        assert_eq!(envelope_for(Some(Family::Gemini), "/v1beta/openai/chat/completions"), Envelope::OpenAi);
        assert_eq!(envelope_for(Some(Family::Ollama), "/api/chat"), Envelope::Ollama);
        assert_eq!(envelope_for(Some(Family::Ollama), "/v1/chat/completions"), Envelope::OpenAi);
        assert_eq!(envelope_for(None, "/x"), Envelope::Gateway);
    }
}
