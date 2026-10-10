//! Anthropic's Files and Message Batches through the gateway (concept section 8.3).
//!
//! Anthropic tells files and batches apart by credential only, and credentials are usually
//! shared between workspaces. The gateway therefore records which workspace created each
//! object (`provider_objects`), serves lists from that record, and answers 404 for another
//! workspace's ids. Each batch item goes through the same resolution, merging and filtering
//! as a single message, and the limits are checked when the batch is created. When a batch has
//! ended, [`settle_batches`] reads its results and writes one usage event per item at the batch
//! rate.
//!
//! The provider of a file call, which names no model, is the one `x-gw-provider` names, or
//! else the only Anthropic provider the workspace has a model at. Every object is used with the
//! credential of the scope that created it: the shared one, the organisation's or the
//! workspace's.

use std::collections::{BTreeMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use http_body_util::BodyExt;
use serde::Deserialize;
use serde_json::value::RawValue;
use serde_json::{Value, json};

use super::Gw;
use super::auth::{Caller, authenticate};
use super::client_ip::ClientIp;
use super::common;
use super::errors::{Envelope, GwError};
use super::files;
use super::managed::{Interface, Wire, plan};
use crate::extract::{Dialect, parse_whole};
use crate::limits::{Applicable, Subjects};
use crate::model::{AuthScheme, Family, Mode, Status};
use crate::relay::CallRecorder;
use crate::snapshot::{CredentialOwner, ProviderRt, Secret, Snapshot};
use crate::store::objects::ProviderObject;
use crate::telemetry::trace::TraceContext;

/// The largest file Anthropic accepts.
const MAX_FILE_BYTES: u64 = 500 << 20;
/// The largest batch Anthropic accepts.
const MAX_BATCH_BYTES: usize = 256 << 20;
/// The most items Anthropic accepts in one batch.
const MAX_BATCH_ITEMS: usize = 100_000;
/// How long one call to the provider about an object may take.
const OBJECT_TIMEOUT: Duration = Duration::from_secs(120);

// --- the common part ----------------------------------------------------------------------

/// A call about a file or batch: who makes it, and its record in the usage log.
struct ObjectCall {
    caller: Caller,
    rec: CallRecorder,
}

fn error(e: GwError, rec: &CallRecorder) -> Response {
    e.response(Envelope::Anthropic, &rec.request_id())
}

/// Authenticates the caller and starts the call's usage event.
fn begin(gw: &Arc<Gw>, client: IpAddr, headers: &HeaderMap, path: &str) -> Result<ObjectCall, Box<Response>> {
    let trace = TraceContext::from_traceparent(headers.get("traceparent").and_then(|v| v.to_str().ok()));
    let mut rec = CallRecorder::new(gw.clone(), trace);
    let caller = match authenticate(gw, headers, Some(client), Mode::Managed) {
        Ok(c) => c,
        Err(e) => {
            crate::telemetry::metrics::record_rejection(e.kind);
            gw.live.record_rejection(e.kind);
            return Err(Box::new(error(e, &rec)));
        }
    };
    rec.event.organisation_id = caller.key.organisation_id.clone();
    rec.event.workspace_id = caller.key.workspace_id.clone();
    rec.event.key_id = caller.key.id.clone();
    rec.set_mode("managed");
    rec.event.interface = "anthropic".into();
    rec.event.path = path.into();
    // These calls carry no tokens; nothing is estimated for them.
    rec.usage_policy = crate::model::UsagePolicy::Require;
    Ok(ObjectCall { caller, rec })
}

/// Ends a call that failed before or at the provider.
fn fail(e: GwError, mut rec: CallRecorder) -> Response {
    let outcome = if e.source == "upstream" {
        pacod_gw_events::usage::status::ERROR
    } else {
        pacod_gw_events::usage::status::REJECTED
    };
    rec.outcome(outcome, e.status.as_u16(), e.kind, &e.message);
    error(e, &rec)
}

/// Whose credential a scope names, as stored with an object.
fn scope_name(owner_scope: Scope) -> &'static str {
    match owner_scope {
        Scope::Shared => "shared",
        Scope::Organisation => "organisation",
        Scope::Workspace => "workspace",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Shared,
    Organisation,
    Workspace,
}

/// The credential a new object is created with, and its scope: the workspace's own, else its
/// organisation's, else the shared one.
fn current_credential(p: &ProviderRt, ws: &str, org: &str) -> Option<(Arc<Secret>, Scope)> {
    if let Some(s) = p.credentials.workspaces.get(ws) {
        return Some((s.clone(), Scope::Workspace));
    }
    if let Some(s) = p.credentials.organisations.get(org) {
        return Some((s.clone(), Scope::Organisation));
    }
    p.credentials.global.clone().map(|s| (s, Scope::Shared))
}

/// The credential of the scope that created an object.
fn credential_of(p: &ProviderRt, o: &ProviderObject) -> Option<Arc<Secret>> {
    match o.credential_scope.as_str() {
        "workspace" => p.credentials.workspaces.get(&o.workspace_id).cloned(),
        "organisation" => p.credentials.organisations.get(&o.organisation_id).cloned(),
        _ => p.credentials.global.clone(),
    }
}

/// The provider of a call that names no model: the one `x-gw-provider` names, or else the
/// only Anthropic provider the workspace has a model at.
fn provider_for(caller: &Caller, headers: &HeaderMap) -> Result<Arc<ProviderRt>, GwError> {
    let snap = &caller.snapshot;
    let usable = |p: &ProviderRt| p.doc.family == Family::Anthropic && p.doc.status == Status::Active;
    if let Some(name) = headers.get("x-gw-provider").and_then(|v| v.to_str().ok()) {
        return snap.provider_by_name(name.trim()).filter(|p| usable(p)).cloned().ok_or_else(|| {
            GwError::invalid_request(format!("\"{name}\" is not an Anthropic provider of this installation"))
        });
    }
    let mut found: Vec<&Arc<ProviderRt>> = Vec::new();
    for m in crate::resolve::listed_models(snap, &caller.workspace, &caller.key) {
        if let Some(p) = snap.providers.get(&m.doc.provider_id)
            && usable(p)
            && !found.iter().any(|f| f.id == p.id)
        {
            found.push(p);
        }
    }
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(GwError::invalid_request("this workspace has no model at an Anthropic provider")),
        _ => Err(GwError::invalid_request(
            "this workspace has models at several Anthropic providers: name one with the x-gw-provider header",
        )),
    }
}

/// The headers of a call to the provider about an object.
fn object_headers(
    incoming: &HeaderMap,
    p: &ProviderRt,
    secret: Option<&Secret>,
    snap: &Snapshot,
    body_len: Option<u64>,
) -> Result<HeaderMap, GwError> {
    common::check_anthropic_betas(incoming, snap)?;
    let scheme = p.doc.managed_auth_scheme();
    let mut h = common::managed_headers(
        incoming,
        &["anthropic-beta"],
        scheme,
        secret.map(|s| s.value.as_str()),
        body_len.unwrap_or(0) as usize,
        false,
    )?;
    if body_len.is_none() {
        h.remove(CONTENT_LENGTH);
    }
    let version = incoming.get("anthropic-version").cloned().unwrap_or(HeaderValue::from_static("2023-06-01"));
    h.insert("anthropic-version", version);
    Ok(h)
}

/// One call to the provider. The response comes back as it is.
async fn send(
    gw: &Gw,
    method: Method,
    url: String,
    headers: HeaderMap,
    body: Body,
    rec: &mut CallRecorder,
) -> Result<(http::response::Parts, hyper::body::Incoming), GwError> {
    let mut request = http::Request::builder().method(method).uri(&url).body(body).map_err(GwError::internal)?;
    *request.headers_mut() = headers;
    if let Some(host) = request.uri().authority().map(|a| a.as_str().to_owned())
        && let Ok(v) = HeaderValue::from_str(&host)
    {
        request.headers_mut().insert(http::header::HOST, v);
    }
    rec.timeline.attempt();
    rec.event.attempts = rec.event.attempts.saturating_add(1);
    let sent = gw.upstream.send(request, OBJECT_TIMEOUT).await.map_err(|e| match e {
        crate::upstream::SendError::Timeout(d) => {
            GwError::upstream_timeout(format!("the provider did not answer within {d:?}"))
        }
        crate::upstream::SendError::Egress(m) => GwError::not_allowed(m),
        other => GwError::upstream_error(other.to_string()),
    })?;
    rec.timeline.upstream_head(sent.head_at);
    let (parts, body) = sent.response.into_parts();
    rec.event.http_status = parts.status.as_u16();
    if let Some(id) = common::upstream_request_id(&parts.headers) {
        rec.event.upstream_request_id = id.chars().take(128).collect();
    }
    Ok((parts, body))
}

/// A whole provider answer as JSON, at most `limit` bytes.
async fn whole(body: hyper::body::Incoming, limit: usize) -> Result<Bytes, GwError> {
    let collected = http_body_util::Limited::new(body, limit)
        .collect()
        .await
        .map_err(|_| GwError::upstream_error("the provider's answer could not be read"))?;
    Ok(collected.to_bytes())
}

/// The response to the caller: the provider's status and body, with JSON or the given type.
fn reply(status: StatusCode, content_type: &str, body: Body, rec: &CallRecorder) -> Response {
    let mut r = Response::new(body);
    *r.status_mut() = status;
    if let Ok(v) = HeaderValue::from_str(content_type) {
        r.headers_mut().insert(CONTENT_TYPE, v);
    }
    if let Ok(v) = HeaderValue::from_str(&rec.request_id()) {
        r.headers_mut().insert("x-gw-request-id", v);
    }
    r
}

/// Ends a call the provider answered, and passes its answer on.
fn relay(status: StatusCode, content_type: &str, body: Body, mut rec: CallRecorder) -> Response {
    if status.is_success() {
        rec.outcome(pacod_gw_events::usage::status::OK, status.as_u16(), "", "");
    } else {
        rec.outcome(
            pacod_gw_events::usage::status::ERROR,
            status.as_u16(),
            "upstream_error",
            "the provider answered with an error",
        );
    }
    let response = reply(status, content_type, body, &rec);
    rec.finish();
    response
}

/// The caller's object, by id and kind, or 404: another workspace's objects are not confirmed.
async fn owned(gw: &Gw, caller: &Caller, id: &str, kind: &str) -> Result<ProviderObject, GwError> {
    let missing = || {
        GwError::not_found(
            if kind == "file" { "file_not_found" } else { "batch_not_found" },
            format!("no {kind} \"{}\" in this workspace", id.chars().take(100).collect::<String>()),
        )
    };
    let o = gw.config.store().object(id).await.map_err(|_| {
        GwError::unavailable("object_check_unavailable", "the gateway could not look up the object's owner")
    })?;
    match o {
        Some(o) if o.kind == kind && o.workspace_id == caller.workspace.id && o.status != "deleted" => Ok(o),
        _ => Err(missing()),
    }
}

/// The provider and credential an object was created with.
fn provider_of<'a>(
    snap: &'a Snapshot,
    o: &ProviderObject,
) -> Result<(&'a Arc<ProviderRt>, Option<Arc<Secret>>), GwError> {
    let p =
        snap.providers.get(&o.provider_id).filter(|p| p.doc.status == Status::Active).ok_or_else(|| {
            GwError::unavailable("provider_unavailable", "the provider of this object is not available")
        })?;
    let secret = credential_of(p, o);
    if secret.is_none() && p.doc.managed_auth_scheme() != AuthScheme::None {
        return Err(GwError::unavailable(
            "provider_unavailable",
            "the credential this object was created with is no longer configured",
        ));
    }
    Ok((p, secret))
}

fn content_type_of(parts: &http::response::Parts) -> String {
    parts.headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("application/json").to_owned()
}

// --- files --------------------------------------------------------------------------------

/// The refusal of a call to `/v1/files` without Anthropic's version header.
fn without_anthropic(headers: &HeaderMap) -> Option<Response> {
    if headers.contains_key("anthropic-version") {
        return None;
    }
    // OpenAI's file API goes through proxy mode.
    Some(
        GwError::not_found(
            "not_found",
            "the managed /v1/files is Anthropic's Files API and needs the anthropic-version header; \
         OpenAI's files are reached through proxy mode",
        )
        .response(Envelope::Anthropic, ""),
    )
}

/// `POST /v1/files`: the upload is relayed as it arrives, and the new file recorded.
pub async fn files_create(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    if let Some(r) = without_anthropic(&parts.headers) {
        return r;
    }
    let ObjectCall { caller, mut rec } = match begin(&gw, client, &parts.headers, "/v1/files") {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let length = parts.headers.get(CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    if length.is_some_and(|n| n > MAX_FILE_BYTES) {
        return fail(GwError::invalid_request("the file is larger than 500 MB"), rec);
    }
    let result = async {
        let provider = provider_for(&caller, &parts.headers)?;
        rec.event.provider = provider.doc.name.clone();
        let (secret, scope) = current_credential(&provider, &caller.workspace.id, &caller.workspace.organisation_id)
            .map(|(s, scope)| (Some(s), scope))
            .unwrap_or((None, Scope::Shared));
        if secret.is_none() && provider.doc.managed_auth_scheme() != AuthScheme::None {
            return Err(GwError::unavailable("provider_unavailable", "no credential is configured for this provider"));
        }
        let mut headers = object_headers(&parts.headers, &provider, secret.as_deref(), &caller.snapshot, length)?;
        // The multipart body keeps its type and boundary.
        if let Some(ct) = parts.headers.get(CONTENT_TYPE) {
            headers.insert(CONTENT_TYPE, ct.clone());
        }
        let capped = Body::new(http_body_util::Limited::new(body, MAX_FILE_BYTES as usize));
        let (up, incoming) =
            send(&gw, Method::POST, provider.doc.managed_url("v1/files"), headers, capped, &mut rec).await?;
        let bytes = whole(incoming, 1 << 20).await?;
        if up.status.is_success() {
            let object: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            if let Some(id) = object["id"].as_str() {
                let record = ProviderObject {
                    id: id.to_owned(),
                    provider_id: provider.id.clone(),
                    kind: "file".into(),
                    organisation_id: caller.workspace.organisation_id.clone(),
                    workspace_id: caller.workspace.id.clone(),
                    key_id: caller.key.id.clone(),
                    credential_scope: scope_name(scope).into(),
                    status: "active".into(),
                    object: object.clone(),
                    detail: json!({}),
                    cost: None,
                    currency: String::new(),
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                    settled_at: None,
                };
                gw.config.store().object_insert(&record).await.map_err(|_| {
                    GwError::unavailable("object_record_failed", "the file was uploaded but could not be recorded")
                })?;
            }
        }
        Ok((up.status, content_type_of(&up), bytes))
    }
    .await;
    match result {
        Ok((status, ct, bytes)) => relay(status, &ct, Body::from(bytes), rec),
        Err(e) => fail(e, rec),
    }
}

#[derive(Default)]
struct ListQuery {
    limit: Option<i64>,
    after_id: Option<String>,
    before_id: Option<String>,
    /// The Files API's cursor: here the id of the last file of the previous page.
    page: Option<String>,
}

/// A list in the shapes both APIs use: `data`, `has_more`, `first_id`, `last_id`, and Files'
/// `next_page`.
async fn list_objects(gw: &Gw, caller: &Caller, kind: &str, uri: &http::Uri) -> Result<Value, GwError> {
    let mut q = ListQuery::default();
    for (k, v) in url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes()) {
        match k.as_ref() {
            "limit" => {
                q.limit =
                    Some(v.parse().map_err(|_| GwError::invalid_request("limit must be a number from 1 to 1000"))?)
            }
            "after_id" => q.after_id = Some(v.into_owned()),
            "before_id" => q.before_id = Some(v.into_owned()),
            "page" => q.page = Some(v.into_owned()),
            _ => {}
        }
    }
    let limit = q.limit.unwrap_or(20).clamp(1, 1000);
    let after = q.after_id.or(q.page);
    let mut rows = gw
        .config
        .store()
        .objects_of(&caller.workspace.id, kind, limit + 1, after.as_deref(), q.before_id.as_deref(), false)
        .await
        .map_err(|_| GwError::unavailable("object_list_unavailable", "the list could not be read"))?;
    let has_more = rows.len() as i64 > limit;
    if q.before_id.is_some() && after.is_none() {
        // Newest first: the extra row of a page before an id is at the front.
        if has_more {
            rows.remove(0);
        }
    } else {
        rows.truncate(limit as usize);
    }
    let data: Vec<Value> = rows.iter().map(|o| outgoing(&o.object)).collect();
    let first = data.first().map(|o| o["id"].clone()).unwrap_or(Value::Null);
    let last = data.last().map(|o| o["id"].clone()).unwrap_or(Value::Null);
    let next_page = if has_more { last.clone() } else { Value::Null };
    Ok(json!({ "data": data, "has_more": has_more, "first_id": first, "last_id": last, "next_page": next_page }))
}

/// `GET /v1/files`: the workspace's files, from the gateway's record.
pub async fn files_list(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    let (parts, _) = req.into_parts();
    if let Some(r) = without_anthropic(&parts.headers) {
        return r;
    }
    let ObjectCall { caller, rec } = match begin(&gw, client, &parts.headers, "/v1/files") {
        Ok(c) => c,
        Err(r) => return *r,
    };
    match list_objects(&gw, &caller, "file", &parts.uri).await {
        Ok(v) => relay(StatusCode::OK, "application/json", Body::from(v.to_string()), rec),
        Err(e) => fail(e, rec),
    }
}

/// `GET /v1/files/{id}`, `GET /v1/files/{id}/content` and `DELETE /v1/files/{id}`, for the
/// workspace's own files.
async fn file_call(gw: Arc<Gw>, client: IpAddr, req: Request, id: String, content: bool) -> Response {
    let (parts, _) = req.into_parts();
    if let Some(r) = without_anthropic(&parts.headers) {
        return r;
    }
    let path = if content { "/v1/files/{id}/content" } else { "/v1/files/{id}" };
    let ObjectCall { caller, mut rec } = match begin(&gw, client, &parts.headers, path) {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let method = parts.method.clone();
    let result = async {
        let o = owned(&gw, &caller, &id, "file").await?;
        let (provider, secret) = provider_of(&caller.snapshot, &o)?;
        rec.event.provider = provider.doc.name.clone();
        let headers = object_headers(&parts.headers, provider, secret.as_deref(), &caller.snapshot, Some(0))?;
        let suffix = if content { "/content" } else { "" };
        let url = provider.doc.managed_url(&format!("v1/files/{id}{suffix}"));
        let (up, incoming) = send(&gw, method.clone(), url, headers, Body::empty(), &mut rec).await?;
        if method == Method::DELETE && up.status.is_success() {
            let _ = gw.config.store().object_deleted(&id).await;
        }
        Ok((up, incoming))
    }
    .await;
    match result {
        // The content streams through as it arrives.
        Ok((up, incoming)) => relay(up.status, &content_type_of(&up), Body::new(incoming), rec),
        Err(e) => fail(e, rec),
    }
}

pub async fn file_get(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    file_call(gw, client, req, id, false).await
}

pub async fn file_content(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    file_call(gw, client, req, id, true).await
}

// --- batches ------------------------------------------------------------------------------

/// A batch object as the caller sees it: `results_url` names the gateway's own path, relative
/// to the address the caller used, so that the SDKs fetch the results through the gateway
/// with the gateway key, and never from the provider.
fn outgoing(object: &Value) -> Value {
    let mut o = object.clone();
    if o["type"] == "message_batch"
        && !o["results_url"].is_null()
        && let Some(id) = o["id"].as_str()
    {
        o["results_url"] = json!(format!("/v1/messages/batches/{id}/results"));
    }
    o
}

#[derive(Deserialize)]
struct BatchItem<'a> {
    custom_id: String,
    #[serde(borrow)]
    params: &'a RawValue,
}

#[derive(Deserialize)]
struct BatchRequest<'a> {
    #[serde(borrow)]
    requests: Vec<BatchItem<'a>>,
}

fn valid_custom_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// `POST /v1/messages/batches`: every item is resolved, merged and filtered like a single
/// message, the limits are checked, and the batch goes to the provider.
pub async fn batches_create(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let ObjectCall { caller, mut rec } = match begin(&gw, client, &parts.headers, "/v1/messages/batches") {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let bytes = match http_body_util::Limited::new(body, MAX_BATCH_BYTES).collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return fail(GwError::invalid_request("the batch is larger than 256 MB"), rec),
    };
    let mut admitted: Vec<Vec<Applicable>> = Vec::new();
    let result = create_batch(&gw, &caller, &parts.headers, &bytes, &mut admitted, &mut rec).await;
    match result {
        Ok((status, ct, answer)) => relay(status, &ct, Body::from(answer), rec),
        Err(e) => {
            // Nothing was created: the requests counted at admission are taken back.
            for a in admitted.iter().flatten() {
                gw.limits.release(a);
            }
            fail(e, rec)
        }
    }
}

async fn create_batch(
    gw: &Arc<Gw>,
    caller: &Caller,
    headers: &HeaderMap,
    bytes: &Bytes,
    admitted: &mut Vec<Vec<Applicable>>,
    rec: &mut CallRecorder,
) -> Result<(StatusCode, String, Bytes), GwError> {
    let request: BatchRequest<'_> = serde_json::from_slice(bytes)
        .map_err(|e| GwError::invalid_request(format!("the batch is not acceptable: {e}")))?;
    if request.requests.is_empty() || request.requests.len() > MAX_BATCH_ITEMS {
        return Err(GwError::invalid_request("a batch holds between 1 and 100,000 requests"));
    }
    let mut seen = HashSet::new();
    let mut provider: Option<Arc<ProviderRt>> = None;
    let mut items = BTreeMap::new();
    let mut out = Vec::with_capacity(bytes.len());
    out.extend_from_slice(b"{\"requests\":[");
    let now = Utc::now();
    for (i, item) in request.requests.iter().enumerate() {
        if !valid_custom_id(&item.custom_id) {
            return Err(GwError::invalid_request(format!(
                "requests[{i}].custom_id must be 1 to 64 letters, digits, hyphens or underscores"
            )));
        }
        if !seen.insert(item.custom_id.as_str()) {
            return Err(GwError::invalid_request(format!(
                "requests[{i}].custom_id \"{}\" is used twice",
                item.custom_id
            )));
        }
        let params = Bytes::copy_from_slice(item.params.get().as_bytes());
        let p = plan(caller, Interface::Anthropic, headers, &params)
            .map_err(|e| GwError::invalid_request(format!("requests[{i}]: {}", e.message)))?;
        if p.shaped.stream {
            return Err(GwError::invalid_request(format!("requests[{i}]: stream is not supported in a batch")));
        }
        if p.wire != Wire::Anthropic {
            return Err(GwError::invalid_request(format!(
                "requests[{i}]: the model \"{}\" is not at an Anthropic provider; a batch goes to one Anthropic provider",
                p.target.model.doc.name
            )));
        }
        match &provider {
            Some(prev) if prev.id != p.target.provider.id => {
                return Err(GwError::invalid_request(format!(
                    "requests[{i}]: the model \"{}\" is at another provider than the batch's first request",
                    p.target.model.doc.name
                )));
            }
            None => provider = Some(p.target.provider.clone()),
            _ => {}
        }
        // The limits, as for a single message; each item counts as a request.
        let subjects = Subjects {
            workspace: &caller.workspace,
            key: &caller.key,
            end_user: p.params.end_user.as_deref().unwrap_or_default(),
            model: &p.target.model.doc.name,
            template: p.target.template.as_ref().map_or("", |t| t.name.as_str()),
            tags: &p.params.tags,
        };
        let mut applicable = gw.limits.applicable(&subjects, now);
        gw.limits.admit(&mut applicable).map_err(|r| GwError::from_refusal(&r))?;
        admitted.push(applicable);

        items.insert(
            item.custom_id.clone(),
            json!({
                "model_id": p.target.model.id,
                "model": p.target.model.doc.name,
                "requested": p.model_requested,
                "template": p.target.template.as_ref().map(|t| t.name.clone()).unwrap_or_default(),
                "template_version": p.target.template.as_ref().map_or(0, |t| t.version),
                "end_user": p.params.end_user.clone().unwrap_or_default(),
                "tags": p.params.tags,
            }),
        );
        if i > 0 {
            out.push(b',');
        }
        out.extend_from_slice(b"{\"custom_id\":");
        out.extend_from_slice(json!(item.custom_id).to_string().as_bytes());
        out.extend_from_slice(b",\"params\":");
        out.extend_from_slice(&p.shaped.body);
        out.push(b'}');
    }
    out.extend_from_slice(b"]}");
    let provider = provider.expect("at least one request");
    rec.event.provider = provider.doc.name.clone();

    let (secret, scope) = current_credential(&provider, &caller.workspace.id, &caller.workspace.organisation_id)
        .map(|(s, scope)| (Some(s), scope))
        .unwrap_or((None, Scope::Shared));
    if secret.is_none() && provider.doc.managed_auth_scheme() != AuthScheme::None {
        return Err(GwError::unavailable("provider_unavailable", "no credential is configured for this provider"));
    }
    let owner = if scope == Scope::Shared { CredentialOwner::Shared } else { CredentialOwner::Own };
    // Files the items name must be this workspace's, as in single messages.
    files::check(gw, caller, owner, &out).await?;

    let headers = object_headers(headers, &provider, secret.as_deref(), &caller.snapshot, Some(out.len() as u64))?;
    let (up, incoming) =
        send(gw, Method::POST, provider.doc.managed_url("v1/messages/batches"), headers, Body::from(out), rec).await?;
    let answer = whole(incoming, 1 << 20).await?;
    if !up.status.is_success() {
        return Err(GwError::upstream_refused(
            up.status,
            format!(
                "the provider refused the batch: {}",
                String::from_utf8_lossy(&answer).chars().take(300).collect::<String>()
            ),
        ));
    }
    let object: Value =
        serde_json::from_slice(&answer).map_err(|_| GwError::upstream_error("the provider's batch is not JSON"))?;
    let id = object["id"].as_str().ok_or_else(|| GwError::upstream_error("the provider's batch has no id"))?;
    let record = ProviderObject {
        id: id.to_owned(),
        provider_id: provider.id.clone(),
        kind: "batch".into(),
        organisation_id: caller.workspace.organisation_id.clone(),
        workspace_id: caller.workspace.id.clone(),
        key_id: caller.key.id.clone(),
        credential_scope: scope_name(scope).into(),
        status: object["processing_status"].as_str().unwrap_or("in_progress").to_owned(),
        object: object.clone(),
        // Priced at creation, as a single call is priced when it starts.
        detail: json!({ "items": items, "priced_at": now }),
        cost: None,
        currency: String::new(),
        created_at: now,
        updated_at: now,
        settled_at: None,
    };
    gw.config
        .store()
        .object_insert(&record)
        .await
        .map_err(|_| GwError::unavailable("object_record_failed", "the batch was created but could not be recorded"))?;
    Ok((up.status, "application/json".into(), Bytes::from(outgoing(&object).to_string())))
}

/// `GET /v1/messages/batches`: the workspace's batches, from the gateway's record.
pub async fn batches_list(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    let (parts, _) = req.into_parts();
    let ObjectCall { caller, rec } = match begin(&gw, client, &parts.headers, "/v1/messages/batches") {
        Ok(c) => c,
        Err(r) => return *r,
    };
    match list_objects(&gw, &caller, "batch", &parts.uri).await {
        Ok(v) => relay(StatusCode::OK, "application/json", Body::from(v.to_string()), rec),
        Err(e) => fail(e, rec),
    }
}

/// What a call about one batch does at the provider.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BatchAction {
    Get,
    Cancel,
    Delete,
    Results,
}

async fn batch_call(gw: Arc<Gw>, client: IpAddr, req: Request, id: String, action: BatchAction) -> Response {
    let (parts, _) = req.into_parts();
    let path = match action {
        BatchAction::Get | BatchAction::Delete => "/v1/messages/batches/{id}",
        BatchAction::Cancel => "/v1/messages/batches/{id}/cancel",
        BatchAction::Results => "/v1/messages/batches/{id}/results",
    };
    let ObjectCall { caller, mut rec } = match begin(&gw, client, &parts.headers, path) {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let result = async {
        let o = owned(&gw, &caller, &id, "batch").await?;
        if action == BatchAction::Delete && o.status == "ended" {
            return Err(GwError::conflict(
                "batch_not_settled",
                "the gateway is still accounting this batch's results; delete it in a minute",
            ));
        }
        let (provider, secret) = provider_of(&caller.snapshot, &o)?;
        rec.event.provider = provider.doc.name.clone();
        let (method, suffix) = match action {
            BatchAction::Get => (Method::GET, ""),
            BatchAction::Delete => (Method::DELETE, ""),
            BatchAction::Cancel => (Method::POST, "/cancel"),
            BatchAction::Results => (Method::GET, "/results"),
        };
        let headers = object_headers(&parts.headers, provider, secret.as_deref(), &caller.snapshot, Some(0))?;
        let url = provider.doc.managed_url(&format!("v1/messages/batches/{id}{suffix}"));
        let (up, incoming) = send(&gw, method, url, headers, Body::empty(), &mut rec).await?;
        if action == BatchAction::Results || !up.status.is_success() {
            // The results stream through as they arrive.
            return Ok((up.status, content_type_of(&up), Body::new(incoming)));
        }
        let answer = whole(incoming, 1 << 20).await?;
        if action == BatchAction::Delete {
            let _ = gw.config.store().object_deleted(&id).await;
            return Ok((up.status, "application/json".into(), Body::from(answer)));
        }
        let object: Value = serde_json::from_slice(&answer).unwrap_or(Value::Null);
        if let Some(status) = object["processing_status"].as_str() {
            let _ = gw.config.store().object_update(&id, status, &object).await;
        }
        Ok((up.status, "application/json".into(), Body::from(outgoing(&object).to_string())))
    }
    .await;
    match result {
        Ok((status, ct, body)) => relay(status, &ct, body, rec),
        Err(e) => fail(e, rec),
    }
}

pub async fn batch_get(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    batch_call(gw, client, req, id, BatchAction::Get).await
}

pub async fn batch_delete(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    batch_call(gw, client, req, id, BatchAction::Delete).await
}

pub async fn batch_cancel(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    batch_call(gw, client, req, id, BatchAction::Cancel).await
}

pub async fn batch_results(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    batch_call(gw, client, req, id, BatchAction::Results).await
}

/// `DELETE /v1/files/{id}` and the batch actions share their routes with `GET`.
pub async fn file_delete(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    file_call(gw, client, req, id, false).await
}

// --- settlement ---------------------------------------------------------------------------

/// A usage event's id for one item of a batch: the same for every run, so that an item
/// settled twice after a crash is stored once.
fn item_event_id(batch_id: &str, custom_id: &str) -> uuid::Uuid {
    let digest = crate::crypto::sha256(format!("{batch_id}/{custom_id}").as_bytes());
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    // A version 8 (custom) UUID of the variant RFC 9562 describes.
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    uuid::Uuid::from_bytes(bytes)
}

/// Asks the provider about the batches not yet settled, at most `limit`, none asked within
/// `min_seconds`, and settles those that have ended. Run by the management instances' upkeep,
/// one at a time. Returns how many batches were settled.
pub async fn settle_batches(gw: &Arc<Gw>, limit: i64, min_seconds: i64) -> anyhow::Result<usize> {
    let store = gw.config.store();
    let mut settled = 0;
    for o in store.batches_to_poll(limit, min_seconds).await? {
        match settle_one(gw, &o).await {
            Ok(true) => settled += 1,
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(batch = %o.id, error = %e, "a batch could not be settled; it is tried again later")
            }
        }
    }
    Ok(settled)
}

async fn settle_one(gw: &Arc<Gw>, o: &ProviderObject) -> anyhow::Result<bool> {
    let snap = gw.config.snapshot();
    let (provider, secret) = provider_of(&snap, o).map_err(|e| anyhow::anyhow!(e.message))?;
    let headers = object_headers(&HeaderMap::new(), provider, secret.as_deref(), &snap, Some(0))
        .map_err(|e| anyhow::anyhow!(e.message))?;
    let mut rec = CallRecorder::new(gw.clone(), TraceContext::from_traceparent(None));
    rec.event.status = pacod_gw_events::usage::status::OK.into();
    let url = provider.doc.managed_url(&format!("v1/messages/batches/{}", o.id));
    let (up, incoming) = send(gw, Method::GET, url, headers.clone(), Body::empty(), &mut rec)
        .await
        .map_err(|e| anyhow::anyhow!(e.message))?;
    // The polls are the gateway's own housekeeping, not calls of the workspace.
    rec.discard();
    let answer = whole(incoming, 1 << 20).await.map_err(|e| anyhow::anyhow!(e.message))?;
    if !up.status.is_success() {
        anyhow::bail!("the provider answered {} about the batch", up.status);
    }
    let object: Value = serde_json::from_slice(&answer)?;
    let status = object["processing_status"].as_str().unwrap_or("in_progress");
    gw.config.store().object_update(&o.id, status, &object).await?;
    if status != "ended" {
        return Ok(false);
    }

    // The results, read as they arrive: one event per item.
    let url = provider.doc.managed_url(&format!("v1/messages/batches/{}/results", o.id));
    let mut rec = CallRecorder::new(gw.clone(), TraceContext::from_traceparent(None));
    let (up, mut body) =
        send(gw, Method::GET, url, headers, Body::empty(), &mut rec).await.map_err(|e| anyhow::anyhow!(e.message))?;
    rec.discard();
    if !up.status.is_success() {
        anyhow::bail!("the provider answered {} for the batch's results", up.status);
    }
    let mut buffer: Vec<u8> = Vec::new();
    let mut total = 0.0;
    // The events are dated when the batch ended, kept between its creation here and now in
    // case the provider's clock disagrees with ours.
    let ended_at = object["ended_at"]
        .as_str()
        .and_then(|t| t.parse::<DateTime<Utc>>().ok())
        .unwrap_or_else(Utc::now)
        .clamp(o.created_at, Utc::now());
    while let Some(frame) = body.frame().await {
        let frame = frame?;
        if let Some(data) = frame.data_ref() {
            buffer.extend_from_slice(data);
        }
        while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = buffer.drain(..=end).collect();
            total += settle_item(gw, &snap, o, provider, &line, ended_at)?;
        }
    }
    if !buffer.iter().all(u8::is_ascii_whitespace) {
        total += settle_item(gw, &snap, o, provider, &buffer, ended_at)?;
    }
    let settled = gw.config.store().batch_settled(&o.id, total, &snap.settings.reporting_currency).await?;
    if settled {
        tracing::info!(batch = %o.id, cost = total, "batch settled");
    }
    Ok(settled)
}

/// Writes the usage event of one result line and counts it in the limits. Returns its cost in
/// the reporting currency.
fn settle_item(
    gw: &Arc<Gw>,
    snap: &Snapshot,
    o: &ProviderObject,
    provider: &ProviderRt,
    line: &[u8],
    ended_at: DateTime<Utc>,
) -> anyhow::Result<f64> {
    use pacod_gw_events::usage::status;
    let line = line.trim_ascii();
    if line.is_empty() {
        return Ok(0.0);
    }
    let result: Value = serde_json::from_slice(line)?;
    let custom_id = result["custom_id"].as_str().unwrap_or_default();
    let item = &o.detail["items"][custom_id];
    let mut e = pacod_gw_events::UsageEvent::new(item_event_id(&o.id, custom_id), gw.boot.instance_id.clone());
    e.timestamp = ended_at;
    e.organisation_id = o.organisation_id.clone();
    e.workspace_id = o.workspace_id.clone();
    e.key_id = o.key_id.clone();
    e.mode = "managed".into();
    e.interface = "anthropic".into();
    e.path = "/v1/messages/batches".into();
    e.provider = provider.doc.name.clone();
    e.model_requested = item["requested"].as_str().unwrap_or_default().to_owned();
    e.model_served = item["model"].as_str().unwrap_or_default().to_owned();
    e.template = item["template"].as_str().unwrap_or_default().to_owned();
    e.template_version = item["template_version"].as_u64().unwrap_or(0) as u32;
    e.end_user = item["end_user"].as_str().unwrap_or_default().to_owned();
    e.tags = serde_json::from_value(item["tags"].clone()).unwrap_or_default();
    e.trace_id = o.id.clone();
    e.attempts = 1;
    e.upstream_request_id = format!("{}/{custom_id}", o.id).chars().take(128).collect();
    match result["result"]["type"].as_str() {
        Some("succeeded") => {
            e.status = status::OK.into();
            e.http_status = 200;
            let message = result["result"]["message"].to_string();
            let found = parse_whole(Dialect::Anthropic, message.as_bytes(), &provider.usage_fields);
            if let Some(m) = found.model {
                e.model_reported = m;
            }
            if let Some(r) = found.finish_reason {
                e.finish_reason = r;
            }
            if found.has_usage {
                e.usage = found.usage;
                e.usage_source = "provider".into();
            }
        }
        other => {
            e.status = if other == Some("canceled") { status::CANCELLED } else { status::ERROR }.into();
            e.error_type = format!("batch_{}", other.unwrap_or("unknown"));
            e.error_message =
                result["result"]["error"]["error"]["message"].as_str().unwrap_or_default().chars().take(500).collect();
        }
    }
    // The price in effect when the batch was created, at its batch rate.
    let priced_at =
        o.detail["priced_at"].as_str().and_then(|t| t.parse::<DateTime<Utc>>().ok()).unwrap_or(o.created_at);
    let model = item["model_id"].as_str().and_then(|id| snap.models.get(id));
    if e.usage_source == "provider" {
        let costed = match model {
            Some(m) if m.doc.free => Some(crate::cost::Costed::free(&snap.settings.reporting_currency)),
            Some(m) => match snap.price_at(&m.id, priced_at) {
                Some(price) => Some(crate::cost::cost(price, &e.usage, "batch")),
                None => Some(crate::cost::Costed::unpriced()),
            },
            None => None,
        };
        if let Some(c) = costed {
            e.cost = c.amount;
            e.currency = c.currency;
            e.price_version = c.price_version;
            e.price_variant = c.variant;
            e.price_missing = c.price_missing;
            if let Some(amount) = e.cost {
                e.reporting_currency = snap.settings.reporting_currency.clone();
                e.cost_reporting = snap.settings.to_reporting(amount, &e.currency);
            }
        }
    } else if e.usage_source.is_empty() {
        e.usage_source = "missing".into();
    }
    // Tokens and spend land in the limits' current windows; the item's request was counted
    // when the batch was created.
    let key = snap.keys.values().find(|k| k.id == o.key_id && k.valid_until.is_none());
    if let (Some(key), Some(ws)) = (key, snap.workspaces.get(&o.workspace_id)) {
        let subjects = Subjects {
            workspace: ws,
            key,
            end_user: &e.end_user,
            model: &e.model_served,
            template: &e.template,
            tags: &e.tags,
        };
        let applicable = gw.limits.applicable(&subjects, Utc::now());
        gw.limits.settle_counted(applicable, &e.usage, e.cost_reporting.or(e.cost));
    }
    if !gw.usage_spool.try_write(serde_json::to_vec(&e)?) {
        anyhow::bail!("the event spool is full");
    }
    Ok(e.cost_reporting.or(e.cost).unwrap_or(0.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn results_urls_name_the_gateway() {
        let ended = json!({ "id": "msgbatch_1", "type": "message_batch", "results_url": "https://api.anthropic.com/v1/messages/batches/msgbatch_1/results" });
        assert_eq!(outgoing(&ended)["results_url"], "/v1/messages/batches/msgbatch_1/results");
        let running = json!({ "id": "msgbatch_1", "type": "message_batch", "results_url": null });
        assert_eq!(outgoing(&running)["results_url"], Value::Null);
    }

    #[test]
    fn item_events_have_stable_ids() {
        assert_eq!(item_event_id("b", "x"), item_event_id("b", "x"));
        assert_ne!(item_event_id("b", "x"), item_event_id("b", "y"));
        assert_eq!(item_event_id("b", "x").get_version_num(), 8);
    }

    #[test]
    fn custom_ids_follow_the_providers_rule() {
        assert!(valid_custom_id("item-1_a"));
        assert!(!valid_custom_id(""));
        assert!(!valid_custom_id("with space"));
        assert!(!valid_custom_id(&"x".repeat(65)));
    }
}
