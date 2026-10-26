//! OpenAI's Files and Batches through the gateway (concept section 8.3), next to Anthropic's
//! (`objects.rs`), whose record, credentials and settlement they share.
//!
//! A batch's requests sit in a file of purpose `batch`, uploaded before the batch is made, and
//! name gateway models (aliases, templates) that the provider does not know. The gateway
//! therefore reads such a file when it is uploaded: each line is resolved, merged and filtered
//! like a single call, must be served by the provider the file goes to, and is written with
//! the provider's model id; what was decided per line is kept with the file. A batch is then
//! made only from such a file, and its limits are checked against what was kept. The file is
//! not downloaded again later: OpenAI does not let every purpose's content be read back.
//!
//! The provider of a call that names no model is the one `x-gw-provider` names, or else the
//! only provider serving these APIs (quirk `openai_objects`) that the workspace has a model at.

use std::collections::{BTreeMap, HashSet};
use std::net::IpAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Path, Request, State};
use axum::http::header::{CONTENT_LENGTH, CONTENT_TYPE};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::response::Response;
use bytes::Bytes;
use chrono::{DateTime, TimeZone, Utc};
use http_body_util::BodyExt;
use serde_json::{Value, json};

use super::Gw;
use super::auth::Caller;
use super::client_ip::ClientIp;
use super::common::{self, GatewayParams};
use super::errors::GwError;
use super::managed::{Interface, Wire, plan, route_input};
use super::objects::{
    ItemAudit, ItemRequest, Lines, ObjectCall, Recording, Scope, begin_as, credential_of, current_credential,
    custom_id_of, fail, item_event, item_resolution, owned_of, read_answer, relay_stream, relay_whole, scope_name,
    send, settle_event, settle_lines, store, whole,
};
use crate::extract::{Dialect, parse_whole};
use crate::json::{Patch, apply, scan};
use crate::limits::{Applicable, Subjects};
use crate::model::{AuthScheme, Family, ModelKind, Status};
use crate::relay::CallRecorder;
use crate::resolve::{ResolveRequest, Target, resolve_routed};
use crate::snapshot::{ProviderRt, Secret, Snapshot};
use crate::store::objects::ProviderObject;
use crate::telemetry::trace::TraceContext;

/// The largest file OpenAI accepts.
const MAX_FILE_BYTES: u64 = 512 << 20;
/// The largest batch input file OpenAI accepts, and what the gateway reads at most.
const MAX_BATCH_FILE_BYTES: usize = 200 << 20;
/// The most requests OpenAI accepts in one batch.
const MAX_BATCH_LINES: usize = 50_000;
/// The endpoints a batch can call through the gateway.
const ENDPOINTS: [&str; 3] = ["/v1/chat/completions", "/v1/responses", "/v1/embeddings"];
/// The statuses after which a batch changes no more.
const ENDED: [&str; 4] = ["completed", "failed", "expired", "cancelled"];

/// The provider of a call that names no model: of the providers serving OpenAI's object APIs
/// that the workspace has a model at, the one `x-gw-provider` names, or else the only one.
fn provider_for(caller: &Caller, headers: &HeaderMap) -> Result<Arc<ProviderRt>, GwError> {
    let snap = &caller.snapshot;
    let usable = |p: &ProviderRt| {
        p.doc.family == Family::Openai && p.doc.quirks.openai_objects && p.doc.status == Status::Active
    };
    let mut found: Vec<&Arc<ProviderRt>> = Vec::new();
    for m in crate::resolve::listed_models(snap, &caller.workspace, &caller.key) {
        if let Some(p) = snap.providers.get(&m.doc.provider_id)
            && usable(p)
            && !found.iter().any(|f| f.id == p.id)
        {
            found.push(p);
        }
    }
    if let Some(name) = headers.get("x-gw-provider").and_then(|v| v.to_str().ok()) {
        let name = name.trim();
        return found.iter().find(|p| p.doc.name == name).map(|p| (*p).clone()).ok_or_else(|| {
            GwError::invalid_request(format!(
                "\"{name}\" is not a provider of OpenAI's Files and Batches this workspace has a model at"
            ))
        });
    }
    match found.as_slice() {
        [one] => Ok((*one).clone()),
        [] => Err(GwError::invalid_request(
            "this workspace has no model at a provider that serves OpenAI's Files and Batches (quirk openai_objects)",
        )),
        _ => Err(GwError::invalid_request(
            "this workspace has models at several providers of OpenAI's Files and Batches: name one with the \
             x-gw-provider header",
        )),
    }
}

/// The headers of a call to the provider about an object.
fn headers_for(
    incoming: &HeaderMap,
    p: &ProviderRt,
    secret: Option<&Secret>,
    body_len: Option<u64>,
    content_type: Option<&HeaderValue>,
) -> Result<HeaderMap, GwError> {
    let scheme = p.doc.managed_auth_scheme();
    let auth = match secret {
        Some(s) => common::key_header(scheme, &s.value).map_err(GwError::internal)?,
        None => None,
    };
    let mut h = common::managed_headers(incoming, &[], &auth, body_len.unwrap_or(0) as usize, false, content_type);
    if body_len.is_none() {
        h.remove(CONTENT_LENGTH);
    }
    Ok(h)
}

/// The credential a new object is made with, or why there is none.
fn credential(caller: &Caller, p: &ProviderRt) -> Result<(Option<Arc<Secret>>, Scope), GwError> {
    let (secret, scope) = current_credential(p, &caller.workspace.id, &caller.workspace.organisation_id)
        .map(|(s, scope)| (Some(s), scope))
        .unwrap_or((None, Scope::Shared));
    if secret.is_none() && p.doc.managed_auth_scheme() != AuthScheme::None {
        return Err(GwError::unavailable("provider_unavailable", "no credential is configured for this provider"));
    }
    Ok((secret, scope))
}

/// The provider and credential an object was made with.
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
            "the credential this object was made with is no longer configured",
        ));
    }
    Ok((p, secret))
}

#[allow(clippy::too_many_arguments)]
fn record(
    caller: &Caller,
    p: &ProviderRt,
    scope: &str,
    kind: &str,
    id: &str,
    status: &str,
    object: Value,
    detail: Value,
) -> ProviderObject {
    let now = Utc::now();
    ProviderObject {
        id: id.to_owned(),
        provider_id: p.id.clone(),
        api: "openai".into(),
        kind: kind.into(),
        organisation_id: caller.workspace.organisation_id.clone(),
        workspace_id: caller.workspace.id.clone(),
        key_id: caller.key.id.clone(),
        credential_scope: scope.into(),
        status: status.into(),
        object,
        detail,
        cost: None,
        currency: String::new(),
        created_at: now,
        updated_at: now,
        settled_at: None,
    }
}

// --- the lines of a batch input file -------------------------------------------------------

/// What the gateway decided about one line, kept with the file for the batch and its
/// settlement.
struct Line {
    custom_id: String,
    item: Value,
    out: Vec<u8>,
    /// What the line resolved to, for its audit record.
    target: Target,
}

/// An embeddings request at the provider: resolved like `/v1/embeddings`, with its model id.
fn embedding_line(
    caller: &Caller,
    headers: &HeaderMap,
    body: &Bytes,
    provider: &ProviderRt,
) -> Result<(Bytes, Value, Target), GwError> {
    let top = scan(body).map_err(|e| GwError::invalid_request(format!("the body is not acceptable JSON: {e}")))?;
    let mut params = GatewayParams::from_headers(headers)?;
    if let Some(e) = top.get("gateway") {
        params.merge_object(&body[e.value.clone()])?;
    }
    let model = top.str(body, "model").unwrap_or_default();
    let req = ResolveRequest { model: &model, template: params.template.as_deref(), provider: None, families: None };
    let input = route_input(&params, "openai", false, body.len(), None);
    let (target, _) = resolve_routed(&caller.snapshot, &caller.workspace, &caller.key, &req, &input)?;
    if target.model.doc.kind != ModelKind::Embedding {
        return Err(GwError::invalid_request(format!(
            "the model \"{}\" is not an embedding model",
            target.model.doc.name
        )));
    }
    if target.provider.id != provider.id {
        return Err(GwError::invalid_request(format!(
            "the model \"{}\" is at another provider than the file's",
            target.model.doc.name
        )));
    }
    let mut patch = Patch::new();
    patch.set("model", &json!(target.model.doc.upstream_id));
    patch.remove("gateway");
    let out = apply(body, &top, &patch)
        .map_err(|e| GwError::invalid_request(format!("the body is not acceptable JSON: {e}")))?
        .to_bytes();
    let item = json!({
        "model_id": target.model.id,
        "model": target.model.doc.name,
        "requested": if model.is_empty() { format!("@{}", params.template.as_deref().unwrap_or_default()) } else { model },
        "template": target.template.as_ref().map(|t| t.name.clone()).unwrap_or_default(),
        "template_version": target.template.as_ref().map_or(0, |t| t.version),
        "end_user": params.end_user.clone().unwrap_or_default(),
        "tags": params.tags,
    });
    Ok((out, item, target))
}

/// One line of a batch input file, written for the provider.
fn rewrite_line(caller: &Caller, headers: &HeaderMap, provider: &ProviderRt, raw: &[u8]) -> Result<Line, GwError> {
    let v: Value = serde_json::from_slice(raw).map_err(|e| GwError::invalid_request(format!("not JSON: {e}")))?;
    let custom_id = v["custom_id"]
        .as_str()
        .filter(|c| !c.is_empty())
        .ok_or_else(|| GwError::invalid_request("custom_id is required"))?;
    if v["method"].as_str() != Some("POST") {
        return Err(GwError::invalid_request("method must be POST"));
    }
    let url = v["url"].as_str().unwrap_or_default();
    if !ENDPOINTS.contains(&url) {
        return Err(GwError::invalid_request(format!("url must be one of {}", ENDPOINTS.join(", "))));
    }
    let body = Bytes::from(v["body"].to_string());
    let (shaped, mut item, target) = match url {
        "/v1/embeddings" => embedding_line(caller, headers, &body, provider)?,
        _ => {
            let iface = if url == "/v1/responses" { Interface::Responses } else { Interface::OpenAi };
            let p = plan(caller, iface, headers, &body, None, None)?;
            if p.shaped.stream {
                return Err(GwError::invalid_request("stream is not supported in a batch"));
            }
            if p.target.provider.id != provider.id || p.wire != Wire::OpenAi || !p.same_dialect {
                return Err(GwError::invalid_request(format!(
                    "the model \"{}\" is not served by the file's provider in OpenAI's own API; a batch goes to one provider",
                    p.target.model.doc.name
                )));
            }
            let item = json!({
                "model_id": p.target.model.id,
                "model": p.target.model.doc.name,
                "requested": p.model_requested,
                "template": p.target.template.as_ref().map(|t| t.name.clone()).unwrap_or_default(),
                "template_version": p.target.template.as_ref().map_or(0, |t| t.version),
                "end_user": p.params.end_user.clone().unwrap_or_default(),
                "tags": p.params.tags,
            });
            (p.shaped.body, item, p.target)
        }
    };
    item["url"] = json!(url);
    let mut out = Vec::with_capacity(shaped.len() + custom_id.len() + 64);
    out.extend_from_slice(b"{\"custom_id\":");
    out.extend_from_slice(json!(custom_id).to_string().as_bytes());
    out.extend_from_slice(b",\"method\":\"POST\",\"url\":");
    out.extend_from_slice(json!(url).to_string().as_bytes());
    out.extend_from_slice(b",\"body\":");
    out.extend_from_slice(&shaped);
    out.extend_from_slice(b"}\n");
    Ok(Line { custom_id: custom_id.to_owned(), item, out, target })
}

/// A batch input file written for the provider, and what was decided per line. Where audit mode
/// records the file's batches, each line it applies to is kept in `audit`.
fn rewrite_batch_file(
    gw: &Gw,
    caller: &Caller,
    headers: &HeaderMap,
    provider: &ProviderRt,
    content: &[u8],
    mut audit: Option<&mut ItemAudit>,
) -> Result<(Vec<u8>, Value), GwError> {
    if caller.workspace.enforces_plugins() {
        return Err(GwError::not_allowed(
            "this workspace checks every call with a plugin, and batches are not checked yet: send the requests one by one",
        ));
    }
    let mut out = Vec::with_capacity(content.len());
    let mut items = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut endpoint: Option<String> = None;
    for (n, raw) in content.split(|b| *b == b'\n').enumerate() {
        let raw = raw.trim_ascii();
        if raw.is_empty() {
            continue;
        }
        if items.len() == MAX_BATCH_LINES {
            return Err(GwError::invalid_request(format!("a batch holds at most {MAX_BATCH_LINES} requests")));
        }
        let line = rewrite_line(caller, headers, provider, raw)
            .map_err(|e| GwError::invalid_request(format!("line {}: {}", n + 1, e.message)))?;
        let url = line.item["url"].as_str().unwrap_or_default().to_owned();
        match &endpoint {
            Some(e) if *e != url => {
                return Err(GwError::invalid_request(format!(
                    "line {}: every request of a batch calls one endpoint, here {e}",
                    n + 1
                )));
            }
            None => endpoint = Some(url),
            _ => {}
        }
        if !seen.insert(line.custom_id.clone()) {
            return Err(GwError::invalid_request(format!(
                "line {}: custom_id \"{}\" is used twice",
                n + 1,
                line.custom_id
            )));
        }
        let mut item = line.item;
        if let Some(a) = audit.as_deref_mut() {
            let end_user = item["end_user"].as_str().unwrap_or_default().to_owned();
            let requested = item["requested"].as_str().unwrap_or_default().to_owned();
            // The limits are checked when the batch is made, not here.
            let resolution =
                item_resolution(gw, caller, &line.target, &requested, &end_user, &item["tags"], "batch", &[]);
            let sent = line.out.trim_ascii_end();
            if a.item(caller, line.target.template.as_deref(), &line.custom_id, &end_user, raw, sent, resolution) {
                item["audited"] = json!(true);
            }
        }
        out.extend_from_slice(&line.out);
        items.insert(line.custom_id, item);
    }
    let endpoint = endpoint.ok_or_else(|| GwError::invalid_request("the batch file holds no request"))?;
    Ok((out, json!({ "batch": { "endpoint": endpoint, "items": items } })))
}

// --- files --------------------------------------------------------------------------------

/// `POST /v1/files`: a batch input file is read and written for the provider; any other file
/// is relayed as it arrives. The new file is recorded.
pub(super) async fn files_create(gw: Arc<Gw>, client: IpAddr, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let ObjectCall { caller, mut rec } = match begin_as(&gw, client, &parts, "/v1/files", "openai") {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let length = parts.headers.get(CONTENT_LENGTH).and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<u64>().ok());
    if length.is_some_and(|n| n > MAX_FILE_BYTES) {
        return fail(GwError::invalid_request("the file is larger than 512 MB"), rec);
    }
    let mut kept: Option<String> = None;
    // Where the lines' requests were kept for their items' records: what this call's own record
    // keeps instead of them, as received and as sent, with their sizes.
    let mut summaries: Option<(Bytes, u64, Bytes, u64)> = None;
    let result = async {
        let provider = provider_for(&caller, &parts.headers)?;
        rec.event.provider = provider.doc.name.clone();
        let (secret, scope) = credential(&caller, &provider)?;
        let content_type = parts.headers.get(CONTENT_TYPE).cloned();
        // A file small enough to be a batch's input is read whole, to see its purpose.
        let (mut upload, detail) = if length.is_none_or(|n| n <= MAX_BATCH_FILE_BYTES as u64) {
            let bytes = http_body_util::Limited::new(body, MAX_BATCH_FILE_BYTES)
                .collect()
                .await
                .map_err(|_| GwError::invalid_request("send a file over 200 MB with its Content-Length"))?
                .to_bytes();
            if let Some(a) = rec.audit.as_mut() {
                a.request_body(&bytes, &parts.headers);
            }
            let ct = content_type.as_ref().and_then(|v| v.to_str().ok()).unwrap_or_default();
            let form =
                pacod_gw_dialects::multipart::Form::parse(ct, &bytes).map_err(super::translate::dialect_error)?;
            if form.text(&bytes, "purpose") == Some("batch") {
                let file = form.part("file").ok_or_else(|| GwError::invalid_request("file is required"))?;
                let mut audit = ItemAudit::new(&caller, &parts.headers);
                let (written, mut detail) = rewrite_batch_file(
                    &gw,
                    &caller,
                    &parts.headers,
                    &provider,
                    &bytes[file.body.clone()],
                    audit.as_mut(),
                )?;
                // The lines' requests are kept before the file exists, so that no batch runs
                // on it without them.
                let edited = form.rewrite(&bytes, &[("file", pacod_gw_dialects::multipart::Edit::Set(&written))]);
                if let Some(a) = audit
                    && let Some(k) = a.keep(&gw, rec.request_id()).await?
                {
                    detail["batch"]
                        .as_object_mut()
                        .expect("an object")
                        .extend(k.detail().as_object().cloned().unwrap_or_default());
                    if k.stored {
                        let empty = serde_json::Map::new();
                        let items = detail["batch"]["items"].as_object().unwrap_or(&empty);
                        let ids: Vec<&str> = items.keys().map(String::as_str).collect();
                        let received = crate::relay::audit::batch_form_summary(ct, &bytes, &ids);
                        let sent = crate::relay::audit::batch_form_summary(ct, &edited, &ids);
                        summaries = Some((received, bytes.len() as u64, sent, edited.len() as u64));
                    }
                    kept = Some(k.key);
                }
                let n = edited.len() as u64;
                (Upload::Whole(edited), (detail, Some(n)))
            } else {
                let n = bytes.len() as u64;
                (Upload::Whole(bytes), (json!({}), Some(n)))
            }
        } else {
            let capped = Body::new(http_body_util::Limited::new(body, MAX_FILE_BYTES as usize));
            // A recorded upload too large to be read whole is kept as it streams through.
            match rec.audit.as_ref().map(|a| crate::relay::audit::RequestTap::new(a.plan.max_body_bytes)) {
                Some(tap) => (Upload::Tapped(tap.wrap(capped), tap), (json!({}), length)),
                None => (Upload::Streamed(capped), (json!({}), length)),
            }
        };
        let (detail, sent_length) = detail;
        let headers = headers_for(&parts.headers, &provider, secret.as_deref(), sent_length, content_type.as_ref())?;
        let url = provider.doc.managed_url("files");
        let whole_body = match &upload {
            Upload::Whole(b) => Some(b.clone()),
            Upload::Tapped(..) | Upload::Streamed(_) => None,
        };
        let body = upload.take_body();
        let answered = send(&gw, Method::POST, url, headers, body, whole_body.as_ref(), &mut rec).await;
        if let Some(key) = kept.as_deref()
            && !answered.as_ref().is_ok_and(|(up, _)| up.status.is_success())
        {
            let _ = store(&gw)?.items_drop(key).await;
        }
        let (up, incoming) = answered?;
        if let (Some(a), Upload::Tapped(_, tap)) = (rec.audit.as_mut(), &upload) {
            a.tapped_request(tap, &parts.headers);
        }
        let bytes = read_answer(incoming, 1 << 20, &mut rec).await?;
        if up.status.is_success() {
            // Its lines' records hold the requests, each naming its end user: this record keeps
            // only what the file was.
            if let (Some(a), Some((received, received_len, sent, sent_len))) = (rec.audit.as_mut(), summaries.take()) {
                a.batch_requests(received, received_len, sent, sent_len);
            }
            let object: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
            if let Some(id) = object["id"].as_str() {
                let r = record(&caller, &provider, scope_name(scope), "file", id, "active", object.clone(), detail);
                store(&gw)?.object_insert(&r).await.map_err(|_| {
                    GwError::unavailable("object_record_failed", "the file was uploaded but could not be recorded")
                })?;
            }
        }
        Ok((up.status, content_type_of(&up.headers), bytes))
    }
    .await;
    match result {
        Ok((status, ct, bytes)) => relay_whole(status, &ct, Some(&bytes), bytes.clone(), rec),
        Err(e) => fail(e, rec),
    }
}

/// An upload's body on its way to the provider: read whole, or streamed through, with a tap
/// when the call is recorded.
enum Upload {
    Whole(Bytes),
    Tapped(Body, crate::relay::audit::RequestTap),
    Streamed(Body),
}

impl Upload {
    /// The streamed body, leaving the tap where it is.
    fn take_body(&mut self) -> Body {
        match self {
            Upload::Whole(b) => Body::from(b.clone()),
            Upload::Tapped(body, _) | Upload::Streamed(body) => std::mem::replace(body, Body::empty()),
        }
    }
}

fn content_type_of(headers: &HeaderMap) -> String {
    headers.get(CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("application/json").to_owned()
}

/// A list in OpenAI's shape, newest first, from the gateway's record: `after` and `limit` as
/// OpenAI reads them, and `purpose` for files.
async fn list(gw: &Gw, caller: &Caller, kind: &str, uri: &http::Uri) -> Result<Value, GwError> {
    let (mut limit, mut after, mut purpose) = (20i64, None::<String>, None::<String>);
    for (k, v) in url::form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes()) {
        match k.as_ref() {
            "limit" => {
                limit = v.parse().map_err(|_| GwError::invalid_request("limit must be a number from 1 to 10000"))?
            }
            "after" => after = Some(v.into_owned()),
            "purpose" => purpose = Some(v.into_owned()),
            _ => {}
        }
    }
    let limit = limit.clamp(1, 10_000);
    let rows = store(gw)?
        .objects_of(&caller.workspace.id, Some("openai"), kind, limit + 1, after.as_deref(), None, false)
        .await
        .map_err(|_| GwError::unavailable("object_list_unavailable", "the list could not be read"))?;
    let has_more = rows.len() as i64 > limit;
    let data: Vec<Value> = rows
        .iter()
        .take(limit as usize)
        .map(|o| o.object.clone())
        .filter(|o| purpose.as_deref().is_none_or(|p| o["purpose"] == p))
        .collect();
    let first = data.first().map(|o| o["id"].clone()).unwrap_or(Value::Null);
    let last = data.last().map(|o| o["id"].clone()).unwrap_or(Value::Null);
    Ok(json!({ "object": "list", "data": data, "first_id": first, "last_id": last, "has_more": has_more }))
}

/// `GET /v1/files`: the workspace's files, from the gateway's record.
pub(super) async fn files_list(gw: Arc<Gw>, client: IpAddr, req: Request) -> Response {
    let (parts, _) = req.into_parts();
    let ObjectCall { caller, rec } = match begin_as(&gw, client, &parts, "/v1/files", "openai") {
        Ok(c) => c,
        Err(r) => return *r,
    };
    match list(&gw, &caller, "file", &parts.uri).await {
        Ok(v) => relay_whole(StatusCode::OK, "application/json", None, Bytes::from(v.to_string()), rec),
        Err(e) => fail(e, rec),
    }
}

/// `GET /v1/files/{id}`, `GET /v1/files/{id}/content` and `DELETE /v1/files/{id}`, for the
/// workspace's own files.
pub(super) async fn file_call(gw: Arc<Gw>, client: IpAddr, req: Request, id: String, content: bool) -> Response {
    let (parts, _) = req.into_parts();
    let path = if content { "/v1/files/{id}/content" } else { "/v1/files/{id}" };
    let ObjectCall { caller, mut rec } = match begin_as(&gw, client, &parts, path, "openai") {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let method = parts.method.clone();
    let result = async {
        let o = owned_of(&gw, &caller, "openai", &id, "file").await?;
        let (provider, secret) = provider_of(&caller.snapshot, &o)?;
        rec.event.provider = provider.doc.name.clone();
        let headers = headers_for(&parts.headers, provider, secret.as_deref(), Some(0), None)?;
        let suffix = if content { "/content" } else { "" };
        let url = provider.doc.managed_url(&format!("files/{id}{suffix}"));
        let (up, incoming) = send(&gw, method.clone(), url, headers, Body::empty(), None, &mut rec).await?;
        if method == Method::DELETE
            && up.status.is_success()
            && let Some(store) = gw.objects.as_deref()
        {
            let _ = store.object_deleted(&id).await;
            // Its batches' items settled later are recorded with their results only.
            if let Some(key) = o.detail["batch"]["audit_items"].as_str() {
                let _ = store.items_drop(key).await;
            }
        }
        Ok((up, incoming))
    }
    .await;
    match result {
        Ok((up, incoming)) => relay_stream(&gw, up.status, &content_type_of(&up.headers), incoming, content, rec),
        Err(e) => fail(e, rec),
    }
}

// --- batches ------------------------------------------------------------------------------

/// Records the files a batch's provider made for it (its output and its errors) as the
/// batch's workspace's, so that the caller can read them through the gateway.
async fn record_outputs(gw: &Gw, batch: &ProviderObject, object: &Value) {
    let Some(store) = gw.objects.as_deref() else { return };
    for key in ["output_file_id", "error_file_id"] {
        if let Some(id) = object[key].as_str().filter(|id| !id.is_empty()) {
            let now = Utc::now();
            let file = ProviderObject {
                id: id.to_owned(),
                provider_id: batch.provider_id.clone(),
                api: "openai".into(),
                kind: "file".into(),
                organisation_id: batch.organisation_id.clone(),
                workspace_id: batch.workspace_id.clone(),
                key_id: batch.key_id.clone(),
                credential_scope: batch.credential_scope.clone(),
                status: "active".into(),
                object: json!({ "id": id, "object": "file", "purpose": "batch_output", "created_at": now.timestamp() }),
                detail: json!({ "batch_output": batch.id }),
                cost: None,
                currency: String::new(),
                created_at: now,
                updated_at: now,
                settled_at: None,
            };
            // A second record of the same id keeps the first.
            let _ = store.object_insert(&file).await;
        }
    }
}

/// `POST /v1/batches`: from a batch input file the gateway read, with its limits checked per
/// request, on the provider and credential the file was uploaded with.
pub async fn batches_create(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let ObjectCall { caller, mut rec } = match begin_as(&gw, client, &parts, "/v1/batches", "openai") {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let bytes = match http_body_util::Limited::new(body, 1 << 20).collect().await {
        Ok(b) => b.to_bytes(),
        Err(_) => return fail(GwError::invalid_request("the request is larger than 1 MB"), rec),
    };
    if let Some(a) = rec.audit.as_mut() {
        a.request_body(&bytes, &parts.headers);
    }
    let mut admitted: Vec<Vec<Applicable>> = Vec::new();
    match create_batch(&gw, &caller, &parts.headers, &bytes, &mut admitted, &mut rec).await {
        Ok((status, answer)) => relay_whole(status, "application/json", Some(&answer), answer.clone(), rec),
        Err(e) => {
            // Nothing was made: the requests counted at admission are taken back.
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
) -> Result<(StatusCode, Bytes), GwError> {
    if caller.workspace.enforces_plugins() {
        return Err(GwError::not_allowed(
            "this workspace checks every call with a plugin, and batches are not checked yet: send the requests one by one",
        ));
    }
    let request: Value =
        serde_json::from_slice(bytes).map_err(|e| GwError::invalid_request(format!("the request is not JSON: {e}")))?;
    let file_id =
        request["input_file_id"].as_str().ok_or_else(|| GwError::invalid_request("input_file_id is required"))?;
    let file = owned_of(gw, caller, "openai", file_id, "file").await?;
    let batch = file.detail.get("batch").filter(|b| b.is_object()).ok_or_else(|| {
        GwError::invalid_request(
            "the input file was not uploaded through the gateway with purpose batch, so its requests were not checked",
        )
    })?;
    let endpoint = batch["endpoint"].as_str().unwrap_or_default();
    if request["endpoint"].as_str() != Some(endpoint) {
        return Err(GwError::invalid_request(format!(
            "endpoint must be {endpoint}, the endpoint of the file's requests"
        )));
    }
    let (provider, secret) = provider_of(&caller.snapshot, &file)?;
    rec.event.provider = provider.doc.name.clone();

    // The limits, as for single calls; each request counts as one.
    let now = Utc::now();
    let empty = serde_json::Map::new();
    for (custom_id, item) in batch["items"].as_object().unwrap_or(&empty) {
        let tags = serde_json::from_value(item["tags"].clone()).unwrap_or_default();
        let subjects = Subjects {
            workspace: &caller.workspace,
            key: &caller.key,
            end_user: item["end_user"].as_str().unwrap_or_default(),
            model: item["model"].as_str().unwrap_or_default(),
            template: item["template"].as_str().unwrap_or_default(),
            tags: &tags,
            intent: None,
            internal: false,
        };
        let mut applicable = gw.limits.applicable(&subjects, now);
        gw.limits
            .admit(&mut applicable)
            .map_err(|r| GwError::invalid_request(format!("{custom_id}: {}", GwError::from_refusal(&r).message)))?;
        admitted.push(applicable);
    }

    let headers = headers_for(headers, provider, secret.as_deref(), Some(bytes.len() as u64), None)?;
    let url = provider.doc.managed_url("batches");
    let (up, incoming) = send(gw, Method::POST, url, headers, Body::from(bytes.clone()), Some(bytes), rec).await?;
    let answer = read_answer(incoming, 1 << 20, rec).await?;
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
    let r = record(
        caller,
        provider,
        &file.credential_scope,
        "batch",
        id,
        object["status"].as_str().unwrap_or("validating"),
        object.clone(),
        // Priced at creation, as a single call is priced when it starts. The items' audit
        // records come from what the file's upload kept.
        json!({ "items": batch["items"], "priced_at": now, "input_file_id": file_id, "endpoint": endpoint,
                "audit_items": batch["audit_items"], "audit_plan": batch["audit_plan"] }),
    );
    store(gw)?
        .object_insert(&r)
        .await
        .map_err(|_| GwError::unavailable("object_record_failed", "the batch was made but could not be recorded"))?;
    Ok((up.status, answer))
}

/// `GET /v1/batches`: the workspace's batches, from the gateway's record.
pub async fn batches_list(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, req: Request) -> Response {
    let (parts, _) = req.into_parts();
    let ObjectCall { caller, rec } = match begin_as(&gw, client, &parts, "/v1/batches", "openai") {
        Ok(c) => c,
        Err(r) => return *r,
    };
    match list(&gw, &caller, "batch", &parts.uri).await {
        Ok(v) => relay_whole(StatusCode::OK, "application/json", None, Bytes::from(v.to_string()), rec),
        Err(e) => fail(e, rec),
    }
}

async fn batch_call(gw: Arc<Gw>, client: IpAddr, req: Request, id: String, cancel: bool) -> Response {
    let (parts, _) = req.into_parts();
    let path = if cancel { "/v1/batches/{id}/cancel" } else { "/v1/batches/{id}" };
    let ObjectCall { caller, mut rec } = match begin_as(&gw, client, &parts, path, "openai") {
        Ok(c) => c,
        Err(r) => return *r,
    };
    let result = async {
        let o = owned_of(&gw, &caller, "openai", &id, "batch").await?;
        let (provider, secret) = provider_of(&caller.snapshot, &o)?;
        rec.event.provider = provider.doc.name.clone();
        let (method, suffix) = if cancel { (Method::POST, "/cancel") } else { (Method::GET, "") };
        let headers = headers_for(&parts.headers, provider, secret.as_deref(), Some(0), None)?;
        let url = provider.doc.managed_url(&format!("batches/{id}{suffix}"));
        let (up, incoming) = send(&gw, method, url, headers, Body::empty(), None, &mut rec).await?;
        let answer = read_answer(incoming, 1 << 20, &mut rec).await?;
        if up.status.is_success() {
            let object: Value = serde_json::from_slice(&answer).unwrap_or(Value::Null);
            if let Some(status) = object["status"].as_str()
                && let Some(store) = gw.objects.as_deref()
            {
                let _ = store.object_update(&id, status, &object).await;
            }
            record_outputs(&gw, &o, &object).await;
        }
        Ok((up.status, answer))
    }
    .await;
    match result {
        Ok((status, answer)) => relay_whole(status, "application/json", Some(&answer), answer.clone(), rec),
        Err(e) => fail(e, rec),
    }
}

pub async fn batch_get(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    batch_call(gw, client, req, id, false).await
}

pub async fn batch_cancel(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(id): Path<String>,
    req: Request,
) -> Response {
    batch_call(gw, client, req, id, true).await
}

// --- settlement ---------------------------------------------------------------------------

/// Asks the provider about one batch and, once it has ended, writes one usage event per
/// request from its output and error files. Returns whether it was settled.
pub(super) async fn settle_one(gw: &Arc<Gw>, o: &ProviderObject) -> anyhow::Result<bool> {
    let snap = gw.config.snapshot();
    let (provider, secret) = provider_of(&snap, o).map_err(|e| anyhow::anyhow!(e.message))?;
    let headers = headers_for(&HeaderMap::new(), provider, secret.as_deref(), Some(0), None)
        .map_err(|e| anyhow::anyhow!(e.message))?;
    let mut rec = CallRecorder::new(gw.clone(), TraceContext::from_traceparent(None));
    let url = provider.doc.managed_url(&format!("batches/{}", o.id));
    let (up, incoming) = send(gw, Method::GET, url, headers.clone(), Body::empty(), None, &mut rec)
        .await
        .map_err(|e| anyhow::anyhow!(e.message))?;
    // The polls are the gateway's own housekeeping, not calls of the workspace.
    rec.discard();
    let answer = whole(incoming, 1 << 20).await.map_err(|e| anyhow::anyhow!(e.message))?;
    if !up.status.is_success() {
        anyhow::bail!("the provider answered {} about the batch", up.status);
    }
    let object: Value = serde_json::from_slice(&answer)?;
    let status = object["status"].as_str().unwrap_or("in_progress");
    let store = gw.objects.as_deref().ok_or_else(|| anyhow::anyhow!("this gateway keeps no record of objects"))?;
    store.object_update(&o.id, status, &object).await?;
    record_outputs(gw, o, &object).await;
    if !ENDED.contains(&status) {
        return Ok(false);
    }
    let recording = Recording::of(o);
    if recording.as_ref().is_some_and(|r| r.must_wait(gw)) {
        return Ok(false);
    }

    // The batch's end, kept between its creation here and now in case the clocks disagree.
    let ended_at = ["completed_at", "failed_at", "expired_at", "cancelled_at"]
        .iter()
        .find_map(|k| object[*k].as_i64())
        .and_then(|t| Utc.timestamp_opt(t, 0).single())
        .unwrap_or_else(Utc::now)
        .clamp(o.created_at, Utc::now());
    let mut total = 0.0;
    let batch_url = provider.doc.managed_url("batches");
    for (key, failed) in [("output_file_id", false), ("error_file_id", true)] {
        let Some(file) = object[key].as_str().filter(|f| !f.is_empty()) else { continue };
        let url = provider.doc.managed_url(&format!("files/{file}/content"));
        let mut rec = CallRecorder::new(gw.clone(), TraceContext::from_traceparent(None));
        let (up, mut body) = send(gw, Method::GET, url, headers.clone(), Body::empty(), None, &mut rec)
            .await
            .map_err(|e| anyhow::anyhow!(e.message))?;
        rec.discard();
        if !up.status.is_success() {
            anyhow::bail!("the provider answered {} for the batch's {key}", up.status);
        }
        let settle = |line: &[u8], request: ItemRequest<'_>| {
            let url = o.detail["items"][custom_id_of(line.trim_ascii())]["url"].as_str().unwrap_or("/v1/batches");
            let capture = recording.as_ref().and_then(|r| r.capture(o, &batch_url, url, line, request));
            settle_line(gw, &snap, o, provider, line, ended_at, failed, capture)
        };
        let mut buffer: Vec<u8> = Vec::new();
        let mut lines = Lines::default();
        while let Some(frame) = body.frame().await {
            if let Some(data) = frame?.data_ref() {
                buffer.extend_from_slice(data);
            }
            while let Some(end) = buffer.iter().position(|b| *b == b'\n') {
                if lines.push(buffer.drain(..=end).collect()) {
                    total += settle_lines(gw, o, recording.as_ref(), &lines.take(), settle).await?;
                }
            }
        }
        if !buffer.iter().all(u8::is_ascii_whitespace) {
            lines.push(std::mem::take(&mut buffer));
        }
        total += settle_lines(gw, o, recording.as_ref(), &lines.take(), settle).await?;
    }
    let settled = store.batch_settled(&o.id, total, &snap.settings.reporting_currency).await?;
    if settled {
        tracing::info!(batch = %o.id, cost = total, "batch settled");
    }
    Ok(settled)
}

/// One line of a batch's output or error file: its request's usage event. Returns its cost.
#[allow(clippy::too_many_arguments)]
fn settle_line(
    gw: &Arc<Gw>,
    snap: &Snapshot,
    o: &ProviderObject,
    provider: &ProviderRt,
    line: &[u8],
    ended_at: DateTime<Utc>,
    failed: bool,
    capture: Option<crate::relay::audit::AuditCapture>,
) -> anyhow::Result<f64> {
    use pacod_gw_events::usage::status;
    let line = line.trim_ascii();
    if line.is_empty() {
        return Ok(0.0);
    }
    let result: Value = serde_json::from_slice(line)?;
    let custom_id = result["custom_id"].as_str().unwrap_or_default();
    let item = &o.detail["items"][custom_id];
    let mut e = item_event(gw, o, provider, custom_id, ended_at, "openai", "/v1/batches");
    let code = result.pointer("/response/status_code").and_then(Value::as_u64).unwrap_or(0);
    if !failed && (200..300).contains(&code) {
        e.status = status::OK.into();
        e.http_status = code as u16;
        let dialect = match item["url"].as_str() {
            Some("/v1/responses") => Dialect::OpenAiResponses,
            Some("/v1/embeddings") => Dialect::OpenAiEmbeddings,
            _ => Dialect::OpenAiChat,
        };
        let body = result.pointer("/response/body").map(Value::to_string).unwrap_or_default();
        let found = parse_whole(dialect, body.as_bytes(), &provider.usage_fields);
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
    } else {
        e.status = status::ERROR.into();
        e.http_status = code as u16;
        e.error_type = "batch_error".into();
        let message = result
            .pointer("/error/message")
            .or_else(|| result.pointer("/response/body/error/message"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        e.error_message = message.chars().take(500).collect();
    }
    settle_event(gw, snap, o, custom_id, e, capture)
}
