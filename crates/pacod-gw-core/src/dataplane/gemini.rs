//! The Gemini interface of managed mode (concept section 9.1): Gemini's SDKs reach every model
//! a key may use, with only their base URL set to the gateway's root. Both modes of the SDKs
//! are served:
//!
//! - **Gemini's API:** `/v1beta/models/{model}:{method}` (and `/v1/models/…` for an SDK set to
//!   `api_version="v1"`), the model list at `/v1beta/models`.
//! - **Vertex AI:** `/{v1,v1beta1}/projects/{project}/locations/{location}/publishers/{publisher}/models/{model}:{method}`
//!   and the express form `/{v1,v1beta1}/publishers/{publisher}/models/{model}:{method}`. The
//!   project and location a caller names are not used: the provider's own are.
//!
//! A model name can hold `/` and `:` (`openai/gpt-5`, `ollama/llama3:8b`), so the method is what
//! follows the last `:`. In Vertex mode the SDKs write a name with `/` as a publisher and a
//! model (`openai/gpt-5` → `publishers/openai/models/gpt-5`); the gateway reads it back.
//!
//! Files, context caches and Gemini's other objects belong to a provider account: they are
//! reached through proxy mode.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::response::Response;
use pacod_gw_dialects::embeddings::GeminiEmbed;
use serde_json::json;

use super::Gw;
use super::auth::{identify, take_query_key};
use super::client_ip::ClientIp;
use super::errors::{Envelope, GwError};
use super::managed::{self, Interface, PathCall};
use super::openai::{Listed, listed};
use crate::model::ModelKind;

/// `POST /v1beta/models/{model}:{method}` and `GET /v1beta/models/{model}`.
pub async fn model(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(rest): Path<String>,
    req: Request,
) -> Response {
    if req.method() == axum::http::Method::GET {
        return get_model(&gw, client, &rest, req);
    }
    call(gw, client, req, &rest, |m| Ok(m.to_owned())).await
}

/// `POST /v1/models/{model}:{method}`: an SDK set to Gemini's `v1`.
pub async fn v1_model(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(rest): Path<String>,
    req: Request,
) -> Response {
    call(gw, client, req, &rest, |m| Ok(m.to_owned())).await
}

/// `POST /{v1,v1beta1}/projects/{project}/locations/{location}/…`: the SDK's Vertex AI mode.
pub async fn vertex_project(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(rest): Path<String>,
    req: Request,
) -> Response {
    // `{project}/locations/{location}/` comes first; neither is used.
    let mut segments = rest.splitn(4, '/');
    let (_, locations, _, resource) = (segments.next(), segments.next(), segments.next(), segments.next());
    let resource = resource.filter(|_| locations == Some("locations")).unwrap_or_default().to_owned();
    let snapshot = gw.config.snapshot();
    call(gw, client, req, &resource, |path| vertex_model(&snapshot, path)).await
}

/// `POST /{v1,v1beta1}/publishers/{publisher}/models/{model}:{method}`: the SDK's Vertex AI
/// mode with an API key (express mode), or with a base URL and no project.
pub async fn vertex_publisher(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(rest): Path<String>,
    req: Request,
) -> Response {
    let resource = format!("publishers/{rest}");
    let snapshot = gw.config.snapshot();
    call(gw, client, req, &resource, |path| vertex_model(&snapshot, path)).await
}

/// Gemini's objects (files, context caches, …) and anything else under its paths.
pub async fn elsewhere(req: Request) -> Response {
    let path = req.uri().path();
    let what = if path.contains("/files") || path.starts_with("/upload/") {
        "Files"
    } else if path.contains("/cachedContents") {
        "Context caches"
    } else {
        return not_served(&format!("{} {path}", req.method()));
    };
    GwError::not_found(
        "not_found",
        format!("{what} belong to a provider account: reach them through proxy mode, /proxy/{{provider}}{path}"),
    )
    .response(Envelope::Google, "")
}

fn not_served(what: &str) -> Response {
    GwError::not_found(
        "not_found",
        format!("the gateway's Gemini interface does not serve {what}; proxy mode relays Gemini's whole API"),
    )
    .response(Envelope::Google, "")
}

/// The model a Vertex-mode path names, from `publishers/{publisher}/models/{model}` or
/// `models/{model}`; otherwise what the path names, for the answer that it is not served.
fn vertex_model(snapshot: &crate::snapshot::Snapshot, path: &str) -> Result<String, String> {
    if let Some(model) = path.strip_prefix("models/") {
        return Ok(model.to_owned());
    }
    let Some((publisher, model)) = path.strip_prefix("publishers/").and_then(|p| p.split_once("/models/")) else {
        return Err(format!("the resource \"{path}\""));
    };
    // The SDK writes `google/x` as `publishers/google/models/x`, and `x` the same way: a model
    // of the gateway named `google/x` comes first.
    let full = format!("{publisher}/{model}");
    if publisher != "google" || snapshot.model_by_name(&full).is_some() { Ok(full) } else { Ok(model.to_owned()) }
}

/// Serves `{model}:{method}` for the model `name` reads from the path.
async fn call(
    gw: Arc<Gw>,
    client: IpAddr,
    req: Request,
    rest: &str,
    name: impl FnOnce(&str) -> Result<String, String>,
) -> Response {
    let (mut parts, body) = req.into_parts();
    // A gateway key in the query leaves the URI before anything reads it.
    take_query_key(&mut parts);
    let Some((path_model, method)) = rest.rsplit_once(':') else {
        return not_served(&format!("the path \"{rest}\": it names no method (model:method)"));
    };
    let model = match name(path_model) {
        Ok(m) if !m.is_empty() => m,
        Ok(_) => return GwError::invalid_request("the path names no model").response(Envelope::Google, ""),
        Err(what) => return not_served(&what),
    };
    let sse = parts.uri.query().is_some_and(|q| q.split('&').any(|p| p == "alt=sse"));
    let call = PathCall { model, stream: method == "streamGenerateContent", sse };
    let req = Request::from_parts(parts, body);
    match method {
        "generateContent" | "streamGenerateContent" => {
            managed::handle_call(gw, client, req, Interface::Gemini, Some(call)).await
        }
        "countTokens" => managed::gemini_count(gw, client, req, call).await,
        "embedContent" => managed::gemini_embed(gw, client, req, GeminiEmbed::One, call).await,
        "batchEmbedContents" => managed::gemini_embed(gw, client, req, GeminiEmbed::Batch, call).await,
        "predict" => managed::gemini_embed(gw, client, req, GeminiEmbed::Predict, call).await,
        other => not_served(&format!("the method \"{other}\"")),
    }
}

// --- the model list ----------------------------------------------------------------------------

/// A model or template as Gemini's model list describes it.
fn entry(l: &Listed) -> serde_json::Value {
    let methods: &[&str] = match l.model_kind {
        ModelKind::Chat => &["generateContent", "streamGenerateContent", "countTokens"],
        ModelKind::Embedding => &["embedContent", "batchEmbedContents"],
        // Imagen answers `predict`, Gemini's image models `generateContent`; a caller tells
        // them apart by the model's name.
        ModelKind::Image => &["predict", "generateContent"],
        ModelKind::Speech
        | ModelKind::Decision
        | ModelKind::Transcription
        | ModelKind::Rerank
        | ModelKind::Realtime => &[],
    };
    let mut m = json!({
        "name": format!("models/{}", l.id),
        "version": "gateway",
        "displayName": l.display_name,
        "supportedGenerationMethods": methods,
    });
    if let Some(n) = l.context_window {
        m["inputTokenLimit"] = json!(n);
    }
    if let Some(n) = l.max_output_tokens {
        m["outputTokenLimit"] = json!(n);
    }
    m
}

fn json_response(body: &serde_json::Value) -> Response {
    let mut r = Response::new(axum::body::Body::from(body.to_string()));
    r.headers_mut().insert(axum::http::header::CONTENT_TYPE, axum::http::HeaderValue::from_static("application/json"));
    *r.status_mut() = StatusCode::OK;
    r
}

/// `GET /v1beta/models`: the models and templates the key can use, in one page.
pub async fn list_models(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    let (mut parts, _) = req.into_parts();
    take_query_key(&mut parts);
    let caller = match identify(&gw, &parts.headers, Some(client)) {
        Ok(c) => c,
        Err(e) => return e.response(Envelope::Google, ""),
    };
    let models: Vec<serde_json::Value> = listed(&caller).iter().map(entry).collect();
    json_response(&json!({ "models": models }))
}

/// `GET /v1beta/models/{model}`. Model names hold `/` and `:`, so the name is the rest of the path.
fn get_model(gw: &Gw, client: IpAddr, name: &str, req: Request) -> Response {
    let (mut parts, _) = req.into_parts();
    take_query_key(&mut parts);
    let caller = match identify(gw, &parts.headers, Some(client)) {
        Ok(c) => c,
        Err(e) => return e.response(Envelope::Google, ""),
    };
    let name = name.trim_start_matches("models/");
    match listed(&caller).iter().find(|l| l.id == name) {
        Some(l) => json_response(&entry(l)),
        None => GwError::not_found(
            "model_not_found",
            format!("the model \"{name}\" does not exist or is not available to this workspace"),
        )
        .response(Envelope::Google, ""),
    }
}

// --- the same-dialect route --------------------------------------------------------------------

/// How a signature the gateway wrapped for a Gemini caller begins: base64 of `0x00 "gw1"`.
const WRAPPED_MARK: &[u8] = b"\"AGd3M";

/// Members of Vertex AI's own that Gemini's API does not take.
const VERTEX_ONLY: &[&str] = &["labels", "routingConfig", "routing_config", "modelArmorConfig", "model_armor_config"];

/// Whether a Gemini caller's request reaches a Gemini model as the caller's own bytes: on
/// Gemini's API, when the conversation holds no signature of another origin. Vertex AI's
/// signatures and members are the neutral model's to keep apart, so Vertex AI is reached
/// through it.
pub(super) fn same_dialect(bytes: &[u8], target: &crate::resolve::Target) -> bool {
    target.provider.vertex.is_none() && memchr::memmem::find(bytes, WRAPPED_MARK).is_none()
}

/// A reasoning setting compared as Gemini reads it: levels in either case.
fn same_thinking(a: Option<&serde_json::Value>, b: Option<&serde_json::Value>) -> bool {
    let norm = |v: Option<&serde_json::Value>| {
        v.cloned().map(|mut v| {
            if let Some(level) = v.get("thinkingLevel").and_then(serde_json::Value::as_str).map(str::to_ascii_lowercase)
            {
                v["thinkingLevel"] = json!(level);
            }
            v
        })
    };
    norm(a) == norm(b)
}

/// Rewrites a Gemini caller's `generateContent` request for a model on Gemini's API: the
/// parameters the gateway governs in `generationConfig`, the reasoning in the form the model
/// takes, the gateway's own member out. `contents`, `systemInstruction` and `tools` keep the
/// caller's bytes, so that Gemini's prompt cache and its thought signatures stay valid.
pub(super) fn shape_same(
    bytes: &bytes::Bytes,
    top: &crate::json::TopLevel,
    target: &crate::resolve::Target,
    policy: crate::model::UnsupportedParams,
    call: Option<&PathCall>,
) -> Result<managed::Shaped, GwError> {
    use crate::json::{Kind, Op, Patch, apply, scan};
    use crate::model::UnsupportedParams;
    use crate::params::{Decision, MergeContext, merge};
    use pacod_gw_dialects::canonical::{Params, Reasoning, Request as Neutral};

    let model = &target.model.doc;
    let caps = &model.capabilities;
    let mut dropped: Vec<String> = Vec::new();
    let unsupported = |name: &str, what: &str, dropped: &mut Vec<String>| -> Result<(), GwError> {
        if policy == UnsupportedParams::Reject {
            return Err(GwError::unsupported_feature(format!("the model \"{}\" does not take {what}", model.name)));
        }
        dropped.push(name.to_owned());
        Ok(())
    };
    let invalid =
        |e: crate::json::ScanError| GwError::invalid_request(format!("the request body is not acceptable JSON: {e}"));

    // One spelling of `generationConfig`, so that what is governed is what Gemini reads.
    let one = super::shape::gemini_one_spelling(bytes, top).map_err(invalid)?;
    let rescanned = one.as_ref().map(|b| scan(b)).transpose().map_err(invalid)?;
    let (bytes, top) = one.as_ref().zip(rescanned.as_ref()).unwrap_or((bytes, top));

    let mut patch = Patch::new();
    patch.remove("gateway");
    // The path names the model; one named in the body is not Gemini's to choose from.
    patch.remove("model");
    for key in VERTEX_ONLY {
        if top.get(key).is_some() {
            unsupported(&pacod_gw_dialects::gemini::camel(key), &format!("Vertex AI's {key}"), &mut dropped)?;
            patch.remove(*key);
        }
    }

    // --- parameters ---
    let sent = super::shape::read(Some(crate::extract::Dialect::Gemini), bytes, top);
    let cx = MergeContext {
        spec: target.template.as_ref().map(|t| &t.spec),
        capabilities: caps,
        terms: target.terms,
        model_max_output: model.max_output_tokens,
        policy,
        filter_caller: true,
    };
    let decisions = merge(&cx, &sent.params)?;
    let mut notes = super::shape::Notes::default();
    let shape_target = super::shape::Target {
        capabilities: caps,
        quirks: &target.provider.doc.quirks,
        managed: true,
        cache_auto: false,
    };
    super::shape::write(
        Some(crate::extract::Dialect::Gemini),
        &mut patch,
        &decisions,
        &sent,
        &shape_target,
        &mut notes,
    );
    dropped.extend(decisions.dropped.iter().cloned());
    dropped.extend(notes.dropped);
    let mut adjusted = decisions.adjusted.clone();
    adjusted.extend(notes.adjusted);
    let sources = decisions.sources.clone();

    // Reasoning, as this model takes it: a level (Gemini 3) or a token budget (2.5). The
    // caller's wish for thought summaries stays as it was.
    let config = super::shape::inner(bytes, top, "generationConfig");
    let thinking: Option<serde_json::Value> =
        config.as_ref().and_then(|(raw, t)| t.raw(raw, "thinkingConfig")).and_then(|r| serde_json::from_slice(r).ok());
    let decided = |d: &Decision<String>, sent: Option<String>| match d {
        Decision::Keep => sent,
        Decision::Set(v) => Some(v.clone()),
        Decision::Remove => None,
    };
    let budget = match &decisions.reasoning_budget {
        Decision::Keep => sent.params.reasoning_budget,
        Decision::Set(v) => Some(*v),
        Decision::Remove => None,
    };
    let include = thinking.as_ref().and_then(|t| t.get("includeThoughts")).and_then(serde_json::Value::as_bool);
    let neutral = Neutral {
        params: Params {
            reasoning: Reasoning {
                effort: decided(&decisions.reasoning_effort, sent.params.reasoning_effort.clone()),
                budget_tokens: budget,
                summary: Some(include == Some(true)),
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let profile = super::translate::profile_for(target, managed::Wire::Gemini, false);
    let mut written = pacod_gw_dialects::Notes::default();
    let wanted = pacod_gw_dialects::gemini::thinking_config(&neutral, &profile, &mut written);
    if !same_thinking(wanted.as_ref(), thinking.as_ref()) {
        let op = wanted.as_ref().map_or(Op::Remove, |w| Op::Set(bytes::Bytes::from(w.to_string())));
        patch.nested("generationConfig", "thinkingConfig", op);
    }
    for name in written.dropped {
        if !dropped.contains(&name) {
            unsupported(&name, "reasoning settings", &mut dropped)?;
        }
    }
    adjusted.extend(written.adjusted);

    // --- what the capability profile rules out is never sent upstream to fail there ---
    let has = |key: &str| top.get(key).filter(|e| e.kind == Kind::Array && e.value.len() > 2).is_some();
    if (has("tools")) && !caps.tools {
        return Err(GwError::unsupported_feature(format!("the model \"{}\" does not take tools", model.name)));
    }
    let tool_config = ["toolConfig", "tool_config"].into_iter().find(|k| top.get(k).is_some());
    if let Some(key) = tool_config
        && let Some(raw) = top.raw(bytes, key)
        && let Ok(v) = serde_json::from_slice::<serde_json::Value>(raw)
    {
        let calling = v.get("functionCallingConfig").or_else(|| v.get("function_calling_config"));
        let mode = calling.and_then(|c| c.get("mode")).and_then(serde_json::Value::as_str).map(str::to_ascii_uppercase);
        let refused = match mode.as_deref() {
            Some("ANY") if !caps.tool_choice_forced => Some("a forced tool choice"),
            Some("NONE") if !caps.tool_choice_none => Some("the tool choice none"),
            _ => None,
        };
        if let Some(what) = refused {
            unsupported("tool_choice", what, &mut dropped)?;
            patch.remove(key);
        }
    }
    if !caps.structured_output
        && let Some((raw, t)) = &config
    {
        let _ = raw;
        for key in ["responseSchema", "responseJsonSchema"] {
            if t.get(key).is_some() {
                unsupported("response_format", "a JSON schema for its answer", &mut dropped)?;
                patch.nested("generationConfig", key, Op::Remove);
            }
        }
    }
    let contents = top.raw(bytes, "contents").unwrap_or_default();
    if !caps.vision && memchr::memmem::find(contents, b"\"image/").is_some() {
        return Err(GwError::unsupported_feature(format!("the model \"{}\" does not take images", model.name)));
    }
    if !caps.assistant_prefill
        && let Ok(list) = serde_json::from_slice::<Vec<RoleOnly>>(contents)
        && list.last().is_some_and(|c| c.role == "model")
    {
        return Err(GwError::unsupported_feature(format!(
            "the model \"{}\" does not continue a partial model turn; end the contents with a user turn",
            model.name
        )));
    }

    let patched = apply(bytes, top, &patch).map_err(invalid)?;
    Ok(managed::Shaped {
        body: patched.to_bytes(),
        stream: call.is_some_and(|c| c.stream),
        wants_usage: true,
        dropped,
        adjusted,
        sources,
    })
}

/// What the gateway needs to know about a content: whose it is.
#[derive(serde::Deserialize)]
struct RoleOnly {
    #[serde(default)]
    role: String,
}

/// Gemini's stream for a Gemini caller: relayed as server-sent events as they come, or the
/// events' data as the elements of one JSON array for a caller that did not ask for `alt=sse`.
/// A stream that breaks ends with Google's error, so that the caller's SDK reports one.
pub(super) struct PassThrough {
    sse: bool,
    framer: crate::sse::SseFramer,
    written: bool,
    closed: bool,
}

impl PassThrough {
    pub(super) fn new(sse: bool) -> Self {
        Self { sse, framer: crate::sse::SseFramer::new(), written: false, closed: false }
    }

    fn element(out: &mut bytes::BytesMut, written: &mut bool, data: &[u8]) {
        out.extend_from_slice(if *written { b",\r\n" } else { b"[" });
        out.extend_from_slice(data);
        *written = true;
    }

    fn close(&mut self, out: &mut bytes::BytesMut) {
        if !self.sse && !self.closed {
            if !self.written {
                out.extend_from_slice(b"[");
            }
            out.extend_from_slice(b"]");
        }
        self.closed = true;
    }
}

impl crate::relay::Transform for PassThrough {
    fn chunk(&mut self, chunk: &bytes::Bytes, _rec: &mut crate::relay::CallRecorder) -> bytes::Bytes {
        if self.sse {
            return chunk.clone();
        }
        let mut out = bytes::BytesMut::new();
        let written = &mut self.written;
        self.framer.feed(chunk, |ev| Self::element(&mut out, written, &ev.data));
        out.freeze()
    }

    fn end(&mut self, _rec: &mut crate::relay::CallRecorder) -> bytes::Bytes {
        let mut out = bytes::BytesMut::new();
        if !self.sse {
            let written = &mut self.written;
            self.framer.finish(|ev| Self::element(&mut out, written, &ev.data));
        }
        self.close(&mut out);
        out.freeze()
    }

    fn error(&mut self, kind: &str, message: &str, rec: &mut crate::relay::CallRecorder) -> bytes::Bytes {
        let e = if kind == "upstream_timeout" {
            GwError::upstream_timeout(message)
        } else {
            GwError::upstream_error(format!("the provider's stream broke: {message}"))
        };
        let body = serde_json::to_vec(&e.body(Envelope::Google, &rec.request_id())).unwrap_or_default();
        let mut out = bytes::BytesMut::new();
        if self.sse {
            // A complete event, even if the provider stopped inside one.
            out.extend_from_slice(b"\n\ndata: ");
            out.extend_from_slice(&body);
            out.extend_from_slice(b"\n\n");
        } else if !self.closed {
            Self::element(&mut out, &mut self.written, &body);
            self.close(&mut out);
        }
        out.freeze()
    }
}
