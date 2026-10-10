//! Files and message batches created through the gateway, per workspace (concept section 10,
//! "Provider objects"): who created them, their status and, for batches, their cost.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;

use super::AdminState;
use super::auth::Principal;
use super::crud::Page;
use super::error::{ApiError, ApiResult};
use super::openapi::{OpenApi, page_schema};
use crate::store::objects::ProviderObject;

#[derive(Debug, Default, Deserialize)]
struct ObjectsQuery {
    cursor: Option<String>,
    limit: Option<i64>,
}

async fn list(
    st: &AdminState,
    p: &Principal,
    org: &str,
    ws: &str,
    kind: &str,
    q: ObjectsQuery,
) -> ApiResult<Json<Page<ProviderObject>>> {
    let snap = st.config.snapshot();
    let workspace =
        snap.workspaces.get(ws).filter(|w| w.organisation_id == org).ok_or_else(|| ApiError::not_found("workspace"))?;
    if !p.can_read_workspace(&workspace.id, org) {
        return Err(ApiError::forbidden());
    }
    let limit = q.limit.unwrap_or(50).clamp(1, 500);
    // Deleted ones too, with their status: the record of what the workspace did.
    let mut items = st.store.objects_of(ws, kind, limit + 1, q.cursor.as_deref(), None, true).await?;
    let more = items.len() as i64 > limit;
    items.truncate(limit as usize);
    let next_cursor = if more { items.last().map(|o| o.id.clone()) } else { None };
    Ok(Json(Page { items, next_cursor }))
}

async fn files(
    State(st): State<AdminState>,
    p: Principal,
    Path((org, ws)): Path<(String, String)>,
    Query(q): Query<ObjectsQuery>,
) -> ApiResult<Json<Page<ProviderObject>>> {
    list(&st, &p, &org, &ws, "file", q).await
}

async fn batches(
    State(st): State<AdminState>,
    p: Principal,
    Path((org, ws)): Path<(String, String)>,
    Query(q): Query<ObjectsQuery>,
) -> ApiResult<Json<Page<ProviderObject>>> {
    list(&st, &p, &org, &ws, "batch", q).await
}

pub fn routes(router: Router<AdminState>, api: &mut OpenApi) -> Router<AdminState> {
    let schema = api.schema::<ProviderObject>();
    for (kind, id, summary) in [
        ("files", "listProviderFiles", "Files created at a provider through the gateway, newest first"),
        (
            "batches",
            "listProviderBatches",
            "Message batches created through the gateway, with their cost, newest first",
        ),
    ] {
        api.add(
            api.op("get", &format!("/orgs/{{org}}/workspaces/{{ws}}/{kind}"), id, "workspaces", summary)
                .query("cursor", "Continue a listing.")
                .query_int("limit", "Page size, 1 to 500. Default 50.")
                .ok(page_schema(schema.clone())),
        );
    }
    router
        .route("/orgs/{org}/workspaces/{ws}/files", get(files))
        .route("/orgs/{org}/workspaces/{ws}/batches", get(batches))
}
