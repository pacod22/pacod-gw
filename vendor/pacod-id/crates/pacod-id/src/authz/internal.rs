//! The internal API products' services call: the catalogue, organisations
//! and scopes, the snapshot and the changes. A service acts for its own product only. Writes
//! answer with the feed's `seq` after them, so a product can wait until its copy has it.

use std::collections::BTreeMap;
use std::time::Duration;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use tokio::time::Instant;

use super::catalog::{self, Push, PushError};
use super::feed::{self, FeedError};
use super::scopes::{self, OrganisationInput, ScopeError, ScopeInput};
use crate::error::{ApiError, ApiResult};
use crate::http::Shared;
use crate::http::caller::CallerClient;

/// The longest a request for changes waits.
const MAX_WAIT: u64 = 30;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/internal/v1/catalog/{product}", put(push_catalog).get(get_catalog))
        .route("/internal/v1/organisations/{id}", put(put_organisation).delete(delete_organisation))
        .route("/internal/v1/scopes/{kind}/{id}", put(put_scope).delete(delete_scope))
        .route("/internal/v1/authz/snapshot", get(snapshot))
        .route("/internal/v1/authz/changes", get(changes))
        .route("/internal/v1/imports/{product}", get(last_import))
}

/// The product the calling service acts for, when it is `product`.
fn acting_for<'a>(caller: &'a CallerClient, product: Option<&str>) -> ApiResult<&'a str> {
    let own = caller.product.as_deref().filter(|_| caller.is_service()).ok_or_else(|| {
        ApiError::forbidden("only a product's service calls this").with_details(json!({ "code": "not_a_service" }))
    })?;
    if product.is_some_and(|p| p != own) {
        return Err(ApiError::forbidden(format!("{} acts for {own} only", caller.id))
            .with_details(json!({ "code": "wrong_product" })));
    }
    Ok(own)
}

fn seq(seq: i64) -> Json<serde_json::Value> {
    Json(json!({ "seq": seq }))
}

impl From<PushError> for ApiError {
    fn from(e: PushError) -> Self {
        let message = e.to_string();
        match e {
            PushError::Manifest(m) => ApiError::invalid(message).with_details(json!({ "code": m.code() })),
            // As everywhere else: a service acting for another product is refused, not invalid.
            PushError::WrongProduct(_) | PushError::Reserved => {
                ApiError::forbidden(message).with_details(json!({ "code": "wrong_product" }))
            }
            PushError::RoleInUse(roles) => {
                ApiError::conflict(message).with_details(json!({ "code": "role_in_use", "roles": roles }))
            }
            PushError::ScopeTypeInUse(kinds) => {
                ApiError::conflict(message).with_details(json!({ "code": "scope_type_in_use", "types": kinds }))
            }
            PushError::CustomRolesInUse => {
                ApiError::conflict(message).with_details(json!({ "code": "custom_roles_in_use" }))
            }
            PushError::BadMigration(_) => ApiError::invalid(message).with_details(json!({ "code": "bad_migration" })),
            PushError::Store(s) => s.into(),
        }
    }
}

impl From<ScopeError> for ApiError {
    fn from(e: ScopeError) -> Self {
        let message = e.to_string();
        let (status, code) = match &e {
            ScopeError::BadId(_) => (StatusCode::UNPROCESSABLE_ENTITY, "bad_id"),
            ScopeError::CatalogMissing(_) => (StatusCode::CONFLICT, "catalog_missing"),
            ScopeError::NotYourType { .. } => (StatusCode::FORBIDDEN, "wrong_product"),
            ScopeError::BadParent { .. } => (StatusCode::UNPROCESSABLE_ENTITY, "bad_parent"),
            ScopeError::ParentUnknown(_) => (StatusCode::CONFLICT, "scope_unknown"),
            ScopeError::ParentChanged(_) => (StatusCode::CONFLICT, "parent_changed"),
            ScopeError::SlugTaken(_) => (StatusCode::CONFLICT, "slug_taken"),
            ScopeError::System(_) => (StatusCode::CONFLICT, "system_scope"),
            ScopeError::Store(_) => {
                let ScopeError::Store(s) = e else { unreachable!() };
                return s.into();
            }
        };
        ApiError::new(status, "scope_error", message).with_details(json!({ "code": code }))
    }
}

async fn push_catalog(
    State(st): State<Shared>,
    caller: CallerClient,
    Path(product): Path<String>,
    Json(body): Json<Push>,
) -> ApiResult<Response> {
    let product = acting_for(&caller, Some(&product))?.to_owned();
    Ok(Json(catalog::push(&st, &product, body).await?).into_response())
}

async fn get_catalog(
    State(st): State<Shared>,
    caller: CallerClient,
    Path(product): Path<String>,
) -> ApiResult<Response> {
    acting_for(&caller, Some(&product))?;
    let row = st
        .store
        .catalog_rows()
        .await?
        .into_iter()
        .find(|r| r.product == product)
        .ok_or_else(|| ApiError::not_found("no catalogue pushed yet"))?;
    Ok(Json(
        json!({ "product": row.product, "version": row.version, "manifest": row.manifest, "pushed_at": row.pushed_at }),
    )
    .into_response())
}

async fn put_organisation(
    State(st): State<Shared>,
    caller: CallerClient,
    Path(id): Path<String>,
    Json(body): Json<OrganisationInput>,
) -> ApiResult<Response> {
    let product = acting_for(&caller, None)?.to_owned();
    Ok(seq(scopes::put_organisation(&st, &product, &id, &body).await?).into_response())
}

async fn delete_organisation(
    State(st): State<Shared>,
    caller: CallerClient,
    Path(id): Path<String>,
    Query(q): Query<BTreeMap<String, String>>,
) -> ApiResult<Response> {
    let product = acting_for(&caller, q.get("product").map(String::as_str))?.to_owned();
    Ok(seq(scopes::delete_organisation(&st, &product, &id).await?).into_response())
}

async fn put_scope(
    State(st): State<Shared>,
    caller: CallerClient,
    Path((kind, id)): Path<(String, String)>,
    Json(body): Json<ScopeInput>,
) -> ApiResult<Response> {
    let product = acting_for(&caller, None)?.to_owned();
    Ok(seq(scopes::put_scope(&st, &product, &kind, &id, &body).await?).into_response())
}

async fn delete_scope(
    State(st): State<Shared>,
    caller: CallerClient,
    Path((kind, id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let product = acting_for(&caller, None)?.to_owned();
    Ok(seq(scopes::delete_scope(&st, &product, &kind, &id).await?).into_response())
}

#[derive(Deserialize)]
struct ProductQuery {
    product: String,
}

/// The product's last import and the identity writes at pacod-id since about the people it
/// brought, or in its realm: a product rolls back to its own sign-in by this.
async fn last_import(
    State(st): State<Shared>,
    caller: CallerClient,
    Path(product): Path<String>,
) -> ApiResult<Response> {
    let product = acting_for(&caller, Some(&product))?.to_owned();
    let mut conn = st.store.pool().acquire().await?;
    match crate::import::writes_since(&mut conn, &product).await? {
        Some(since) => Ok(Json(since).into_response()),
        None => {
            Err(ApiError::not_found(format!("{product} was never imported"))
                .with_details(json!({ "code": "no_import" })))
        }
    }
}

async fn snapshot(
    State(st): State<Shared>,
    caller: CallerClient,
    Query(q): Query<ProductQuery>,
) -> ApiResult<Response> {
    let product = acting_for(&caller, Some(&q.product))?.to_owned();
    Ok(Json(feed::snapshot(&st, &product).await?).into_response())
}

#[derive(Deserialize)]
struct ChangesQuery {
    product: String,
    after: i64,
    #[serde(default)]
    wait: u64,
}

impl From<FeedError> for ApiError {
    fn from(e: FeedError) -> Self {
        match e {
            FeedError::Gone => {
                ApiError::new(StatusCode::GONE, "gone", e.to_string()).with_details(json!({ "code": "cursor_expired" }))
            }
            FeedError::Store(s) => s.into(),
        }
    }
}

/// The changes after `after`; with `wait`, waits up to that many seconds for one.
async fn changes(State(st): State<Shared>, caller: CallerClient, Query(q): Query<ChangesQuery>) -> ApiResult<Response> {
    let product = acting_for(&caller, Some(&q.product))?.to_owned();
    let deadline = Instant::now() + Duration::from_secs(q.wait.min(MAX_WAIT));
    let mut head = st.feed.subscribe();
    let mut stopping = st.stopping.clone();
    let mut cursor = q.after;
    loop {
        // Subscribed before reading: a change that commits in between still ends the wait.
        let (seen, events) = feed::changes(&st, &product, cursor).await?;
        if !events.is_empty() || Instant::now() >= deadline || *stopping.borrow() {
            return Ok(Json(json!({ "seq": seen, "events": events })).into_response());
        }
        cursor = seen;
        tokio::select! {
            _ = tokio::time::timeout_at(deadline, head.wait_for(|v| *v > cursor)) => {}
            _ = stopping.wait_for(|s| *s) => {}
        }
    }
}
