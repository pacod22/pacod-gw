//! The gateway's own interface (concept section 9.2). This file holds the endpoints that
//! describe what a key can do: the model list and the quota state.

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};

use super::Gw;
use super::auth::{Caller, identify};
use super::client_ip::ClientIp;
use super::errors::{Envelope, GwError};
use crate::model::ModelState;
use crate::resolve::listed_models;
use crate::snapshot::{ModelRt, TemplateRt};

/// A model as the native interface describes it: what the gateway knows that a caller can use.
fn model_entry(caller: &Caller, m: &ModelRt) -> Value {
    let snap = &caller.snapshot;
    let d = &m.doc;
    let terms = snap.grant_terms(&caller.workspace, &m.id);
    let ceiling = [d.max_output_tokens, terms.and_then(|t| t.max_output_tokens)].into_iter().flatten().min();
    json!({
        "id": d.name,
        "display_name": if d.display_name.is_empty() { &d.name } else { &d.display_name },
        "kind": d.kind,
        "provider": snap.providers.get(&d.provider_id).map(|p| p.doc.name.as_str()),
        "context_window": d.context_window,
        "max_output_tokens": ceiling,
        "capabilities": d.capabilities,
        "deprecated": d.state == ModelState::Deprecated,
        "state": d.state.as_str(),
        "modalities": d.modalities(),
        "sunset_at": d.sunset_at,
        "successor": d.successor_id.as_ref().and_then(|s| snap.models.get(s)).map(|s| s.doc.name.as_str()),
    })
}

fn template_entry(caller: &Caller, t: &TemplateRt) -> Value {
    json!({
        "id": format!("@{}", t.name),
        "version": t.version,
        "model": caller.snapshot.models.get(&t.model_id).map(|m| m.doc.name.as_str()),
        "models": t.pool.iter().map(|m| json!({
            "model": m.model.doc.name,
            "tier": m.entry.tier,
            "weight": m.entry.weight,
            "priority": m.entry.priority,
        })).collect::<Vec<_>>(),
        "routing_scheme": t.route.scheme.as_ref().map(|s| &*s.name),
        "max_output_tokens": t.spec.max_output_tokens,
    })
}

/// The templates a key may use, by name.
fn templates_of(caller: &Caller) -> Vec<&Arc<TemplateRt>> {
    let key = &caller.key.doc;
    let mut templates: Vec<_> = caller
        .workspace
        .templates
        .values()
        .filter(|t| key.allowed_templates.is_empty() || key.allowed_templates.iter().any(|a| *a == t.name))
        .collect();
    templates.sort_by(|a, b| a.name.cmp(&b.name));
    templates
}

/// `GET /gw/v1/models`: the models and templates the key can use, with what the gateway
/// knows about each.
pub async fn list_models(State(gw): State<Arc<Gw>>, ClientIp(client): ClientIp, headers: HeaderMap) -> Response {
    let caller = match identify(&gw, &headers, Some(client)) {
        Ok(c) => c,
        Err(e) => return e.response(Envelope::Gateway, ""),
    };
    let models: Vec<Value> = listed_models(&caller.snapshot, &caller.workspace, &caller.key)
        .into_iter()
        .map(|m| model_entry(&caller, m))
        .collect();
    let templates: Vec<Value> = templates_of(&caller).into_iter().map(|t| template_entry(&caller, t)).collect();
    Json(json!({ "models": models, "templates": templates })).into_response()
}

/// `GET /gw/v1/models/{ref}`: one model by its id, or one template as `@name`. Model ids
/// contain slashes, so the reference is the rest of the path.
pub async fn get_model(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Path(reference): Path<String>,
    headers: HeaderMap,
) -> Response {
    let caller = match identify(&gw, &headers, Some(client)) {
        Ok(c) => c,
        Err(e) => return e.response(Envelope::Gateway, ""),
    };
    let found = match reference.strip_prefix('@') {
        Some(name) => templates_of(&caller).into_iter().find(|t| t.name == name).map(|t| {
            let mut v = template_entry(&caller, t);
            // What the template sets, so that an application can show or respect it.
            v["spec"] = serde_json::to_value(&t.spec).unwrap_or(Value::Null);
            v
        }),
        None => listed_models(&caller.snapshot, &caller.workspace, &caller.key)
            .into_iter()
            .find(|m| m.doc.name == reference)
            .map(|m| model_entry(&caller, m)),
    };
    match found {
        Some(v) => Json(v).into_response(),
        None => GwError::not_found(
            "model_not_found",
            format!("\"{reference}\" does not exist or is not available to this workspace"),
        )
        .response(Envelope::Gateway, ""),
    }
}

#[derive(Debug, Default, Deserialize)]
pub struct QuotaQuery {
    /// Show end-user limits for this end user.
    end_user: Option<String>,
}

/// How long the quota endpoint waits for stored totals: two synchronisation rounds.
const QUOTA_MAX_WAIT: std::time::Duration = std::time::Duration::from_millis(2500);

/// `GET /gw/v1/quota`: every limit that applies to the key (and the end user), with what is
/// used, what remains and when the window ends. Applications show it to their users and
/// decide admission at their own boundaries (concept section 12.3).
pub async fn quota(
    State(gw): State<Arc<Gw>>,
    ClientIp(client): ClientIp,
    Query(q): Query<QuotaQuery>,
    headers: HeaderMap,
) -> Response {
    let caller = match identify(&gw, &headers, Some(client)) {
        Ok(c) => c,
        Err(e) => return e.response(Envelope::Gateway, ""),
    };
    let end_user = q
        .end_user
        .or_else(|| headers.get("x-gw-end-user").and_then(|v| v.to_str().ok()).map(str::to_owned))
        .unwrap_or_default();
    // Another gateway may have counted this key's usage: wait for the stored totals of
    // counters this instance has not used yet.
    let applicable = gw.limits.applicable_for(&caller.workspace, &caller.key, end_user.trim(), Utc::now());
    let limits = gw.limits.state_fresh(&applicable, QUOTA_MAX_WAIT).await;
    Json(json!({ "end_user": end_user.trim(), "limits": limits })).into_response()
}
