//! The identity audit log, for holders of `id.audit.read` (`id.admin`, `id.auditor`).

use axum::extract::{Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use pacod_id_authz::ScopeRef;
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;

use super::actor::{Actor, not_allowed};
use crate::error::ApiResult;
use crate::http::Shared;

pub fn routes() -> Router<Shared> {
    Router::new().route("/audit", get(list))
}

#[derive(Deserialize)]
struct ListQuery {
    actor: Option<String>,
    subject: Option<String>,
    action: Option<String>,
    /// Entries older than this id (the previous page's `next`).
    before: Option<i64>,
    limit: Option<i64>,
}

async fn list(State(st): State<Shared>, actor: Actor, Query(q): Query<ListQuery>) -> ApiResult<Response> {
    if !actor.allowed(&st.catalog.get(), "id.audit.read", &[ScopeRef::platform()]) {
        return Err(not_allowed());
    }
    let limit = super::page(q.limit, 100);
    let rows = sqlx::query(
        "SELECT id, at, actor, actor_kind, action, subject, client_id, realm, ip, detail FROM audit_log
         WHERE ($1::text IS NULL OR actor = $1) AND ($2::text IS NULL OR subject = $2) AND ($3::text IS NULL OR action = $3)
           AND ($4::bigint IS NULL OR id < $4)
         ORDER BY id DESC LIMIT $5",
    )
    .bind(&q.actor)
    .bind(&q.subject)
    .bind(&q.action)
    .bind(q.before)
    .bind(limit)
    .fetch_all(st.store.pool())
    .await?;
    let mut entries = Vec::new();
    for r in &rows {
        entries.push(json!({
            "id": r.try_get::<i64, _>("id")?,
            "at": r.try_get::<chrono::DateTime<chrono::Utc>, _>("at")?,
            "actor": r.try_get::<String, _>("actor")?,
            "actor_kind": r.try_get::<String, _>("actor_kind")?,
            "action": r.try_get::<String, _>("action")?,
            "subject": r.try_get::<String, _>("subject")?,
            "client_id": r.try_get::<Option<String>, _>("client_id")?,
            "realm": r.try_get::<Option<String>, _>("realm")?,
            "ip": r.try_get::<String, _>("ip")?,
            "detail": r.try_get::<serde_json::Value, _>("detail")?,
        }));
    }
    let next = (entries.len() as i64 == limit).then(|| entries.last().map(|e| e["id"].clone())).flatten();
    Ok(Json(json!({ "entries": entries, "next": next })).into_response())
}
