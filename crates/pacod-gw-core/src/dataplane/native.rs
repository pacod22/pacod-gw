//! The gateway's own interface (concept section 9.2). This file holds the endpoints that
//! describe what a key can do: the model list and the quota state.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Json;
use axum::extract::{ConnectInfo, Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use chrono::Utc;
use serde::Deserialize;
use serde_json::{Value, json};

use super::Gw;
use super::auth::identify;
use super::errors::Envelope;
use crate::model::ModelState;
use crate::resolve::callable_models;

/// `GET /gw/v1/models`: the models and templates the key can use, with what the gateway
/// knows about each.
pub async fn list_models(
    State(gw): State<Arc<Gw>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let caller = match identify(&gw, &headers, Some(peer.ip())) {
        Ok(c) => c,
        Err(e) => return e.response(Envelope::Gateway, ""),
    };
    let snap = &caller.snapshot;
    let models: Vec<Value> = callable_models(snap, &caller.workspace, &caller.key)
        .into_iter()
        .map(|m| {
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
                "sunset_at": d.sunset_at,
                "successor": d.successor_id.as_ref().and_then(|s| snap.models.get(s)).map(|s| s.doc.name.as_str()),
            })
        })
        .collect();
    let key = &caller.key.doc;
    let mut templates: Vec<_> = caller
        .workspace
        .templates
        .values()
        .filter(|t| key.allowed_templates.is_empty() || key.allowed_templates.iter().any(|a| *a == t.name))
        .collect();
    templates.sort_by(|a, b| a.name.cmp(&b.name));
    let templates: Vec<Value> = templates
        .into_iter()
        .map(|t| {
            json!({
                "id": format!("@{}", t.name),
                "version": t.version,
                "model": snap.models.get(&t.model_id).map(|m| m.doc.name.as_str()),
                "max_output_tokens": t.spec.max_output_tokens,
            })
        })
        .collect();
    Json(json!({ "models": models, "templates": templates })).into_response()
}

#[derive(Debug, Default, Deserialize)]
pub struct QuotaQuery {
    /// Show end-user limits for this end user.
    end_user: Option<String>,
}

/// `GET /gw/v1/quota`: every limit that applies to the key (and the end user), with what is
/// used, what remains and when the window ends. Applications show it to their users and
/// decide admission at their own boundaries (concept section 12.3).
pub async fn quota(
    State(gw): State<Arc<Gw>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Query(q): Query<QuotaQuery>,
    headers: HeaderMap,
) -> Response {
    let caller = match identify(&gw, &headers, Some(peer.ip())) {
        Ok(c) => c,
        Err(e) => return e.response(Envelope::Gateway, ""),
    };
    let end_user = q
        .end_user
        .or_else(|| headers.get("x-gw-end-user").and_then(|v| v.to_str().ok()).map(str::to_owned))
        .unwrap_or_default();
    let limits = gw.limits.states_for(&caller.workspace, &caller.key, end_user.trim(), Utc::now());
    Json(json!({ "end_user": end_user.trim(), "limits": limits })).into_response()
}
