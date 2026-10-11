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
        ModelKind::Speech => &[],
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
