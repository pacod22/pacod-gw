//! Service accounts and their tokens (the gateway's admin tokens). A service account
//! belongs to the platform or an organisation; whoever administers that scope for their product
//! (or holds `id.service_accounts.manage` there) manages it. A token's secret is shown once.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use pacod_id_authz::ScopeRef;
use serde::Deserialize;
use serde_json::json;

use super::actor::{Actor, not_allowed};
use crate::authz::bindings::{self, Protect};
use crate::crypto::tokens;
use crate::error::{ApiError, ApiResult};
use crate::http::Shared;
use crate::store::authz as db;
use crate::store::service::{
    ServiceAccount, service_account_delete, service_account_set_status, service_tokens_revoke,
};
use crate::store::{StoreError, changes};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/service-accounts", get(list).post(create))
        .route("/service-accounts/{id}", get(read).patch(update).delete(remove))
        .route("/service-accounts/{id}/tokens", get(list_tokens).post(create_token))
        .route("/service-tokens/{id}", delete(revoke_token))
}

fn scope_of(organisation: Option<&str>) -> ScopeRef {
    organisation.map_or_else(ScopeRef::platform, ScopeRef::organisation)
}

/// The service account, when the actor manages its scope; 404 otherwise.
async fn managed(st: &Shared, actor: &Actor, id: &str) -> ApiResult<(ServiceAccount, Vec<ScopeRef>)> {
    let sa = st.store.service_account(id).await?.ok_or_else(|| ApiError::not_found("no such service account"))?;
    let mut conn = st.store.pool().acquire().await?;
    let path = db::path_of(&mut conn, &scope_of(sa.organisation_id.as_deref()))
        .await?
        .unwrap_or_else(|| vec![ScopeRef::platform()]);
    if actor.manages_service_accounts(&st.catalog.get(), &path) {
        Ok((sa, path))
    } else {
        Err(ApiError::not_found("no such service account"))
    }
}

#[derive(Deserialize)]
struct ListQuery {
    organisation: Option<String>,
}

async fn list(State(st): State<Shared>, actor: Actor, Query(q): Query<ListQuery>) -> ApiResult<Response> {
    let mut conn = st.store.pool().acquire().await?;
    let path = super::path_of(&mut conn, &scope_of(q.organisation.as_deref())).await?;
    if !actor.manages_service_accounts(&st.catalog.get(), &path) {
        return Err(not_allowed());
    }
    let accounts = st.store.service_accounts_of(q.organisation.as_deref()).await?;
    Ok(Json(json!({ "service_accounts": accounts })).into_response())
}

async fn read(State(st): State<Shared>, actor: Actor, Path(id): Path<String>) -> ApiResult<Response> {
    let (sa, _) = managed(&st, &actor, &id).await?;
    Ok(Json(sa).into_response())
}

fn check_name(name: &str) -> ApiResult<String> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 100 {
        return Err(ApiError::invalid("a name of 1 to 100 characters").with_details(json!({ "code": "bad_name" })));
    }
    Ok(name.to_owned())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewServiceAccount {
    name: String,
    #[serde(default)]
    organisation_id: Option<String>,
}

async fn create(State(st): State<Shared>, actor: Actor, Json(body): Json<NewServiceAccount>) -> ApiResult<Response> {
    let name = check_name(&body.name)?;
    let mut conn = st.store.pool().acquire().await?;
    let path = super::path_of(&mut conn, &scope_of(body.organisation_id.as_deref())).await?;
    if !actor.manages_service_accounts(&st.catalog.get(), &path) {
        return Err(not_allowed());
    }
    let id = tokens::new_id("svc");
    st.store.service_account_create(&id, &name, body.organisation_id.as_deref(), &actor.id).await?;
    st.store.audit(&actor.audit("service_account_created").subject(&id).detail(json!({ "name": name }))).await?;
    let sa = st.store.service_account(&id).await?.ok_or(StoreError::NotFound)?;
    Ok((StatusCode::CREATED, Json(sa)).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    name: Option<String>,
    status: Option<String>,
}

async fn update(
    State(st): State<Shared>,
    actor: Actor,
    Path(id): Path<String>,
    Json(body): Json<Patch>,
) -> ApiResult<Response> {
    let (sa, _) = managed(&st, &actor, &id).await?;
    let mut seq = None;
    // Everything is checked before anything is saved.
    if body.status.as_deref().is_some_and(|s| s != "active" && s != "disabled") {
        return Err(ApiError::invalid("status is active or disabled").with_details(json!({ "code": "bad_status" })));
    }
    let name = body.name.as_deref().map(check_name).transpose()?;
    // The status first: its last-holder check can still refuse, and then nothing is saved.
    if let Some(status) = &body.status
        && *status != sa.status
    {
        let kind = if status == "disabled" { "principal.disabled" } else { "principal.enabled" };
        let mut tx = st.store.begin().await?;
        db::lock_head(&mut tx).await?;
        if status == "disabled" {
            bindings::check_leaving(&mut tx, &st.catalog.get(), &actor.subject, &id).await?;
        }
        service_account_set_status(&mut tx, &id, status).await?;
        seq = Some(
            changes::record(&mut tx, kind, None, Some(&id), json!({ "sub": id, "principal_kind": "service" })).await?,
        );
        tx.commit().await?;
    }
    if let Some(name) = &name {
        st.store.service_account_rename(&id, name).await?;
    }
    st.store.audit(&actor.audit("service_account_updated").subject(&id)).await?;
    let mut body = serde_json::to_value(st.store.service_account(&id).await?.ok_or(StoreError::NotFound)?)
        .map_err(ApiError::internal)?;
    body["seq"] = json!(seq);
    Ok(Json(body).into_response())
}

/// Deletes a service account with its bindings and tokens; refused while it is a protected role's
/// last holder somewhere.
async fn remove(State(st): State<Shared>, actor: Actor, Path(id): Path<String>) -> ApiResult<Response> {
    managed(&st, &actor, &id).await?;
    let catalog = st.catalog.get();
    let mut tx = st.store.begin().await?;
    db::lock_head(&mut tx).await?;
    bindings::check_leaving(&mut tx, &catalog, &actor.subject, &id).await?;
    let held = db::bindings_of(&mut tx, &id).await?;
    bindings::apply(&mut tx, &catalog, Protect::Skip, &held, &[]).await?;
    for token in service_tokens_revoke(&mut tx, &id, None).await? {
        changes::record(&mut tx, "credential.revoked", None, Some(&id), json!({ "token_id": token, "sub": id }))
            .await?;
    }
    service_account_delete(&mut tx, &id).await?;
    let seq = changes::record(
        &mut tx,
        "principal.deleted",
        None,
        Some(&id),
        json!({ "sub": id, "principal_kind": "service" }),
    )
    .await?;
    tx.commit().await?;
    st.store.audit(&actor.audit("service_account_deleted").subject(&id)).await?;
    Ok(Json(json!({ "seq": seq })).into_response())
}

async fn list_tokens(State(st): State<Shared>, actor: Actor, Path(id): Path<String>) -> ApiResult<Response> {
    managed(&st, &actor, &id).await?;
    Ok(Json(json!({ "tokens": st.store.service_tokens_of(&id).await? })).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewToken {
    #[serde(default)]
    expires_at: Option<DateTime<Utc>>,
}

async fn create_token(
    State(st): State<Shared>,
    actor: Actor,
    Path(id): Path<String>,
    Json(body): Json<NewToken>,
) -> ApiResult<Response> {
    let (sa, _) = managed(&st, &actor, &id).await?;
    if sa.status != "active" {
        return Err(ApiError::conflict("the service account is disabled").with_details(json!({ "code": "disabled" })));
    }
    if body.expires_at.is_some_and(|e| e <= Utc::now()) {
        return Err(ApiError::invalid("the expiry is in the past").with_details(json!({ "code": "bad_expiry" })));
    }
    let token_id = tokens::new_id("stk");
    let secret = tokens::token(tokens::SERVICE_TOKEN_PREFIX, &token_id);
    let hint: String = secret.chars().rev().take(4).collect::<Vec<_>>().into_iter().rev().collect();
    st.store
        .service_token_create(&token_id, &id, &st.boot.token_pepper.hash(&secret), None, &hint, body.expires_at)
        .await?;
    st.store.audit(&actor.audit("service_token_created").subject(&id).detail(json!({ "token_id": token_id }))).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({ "id": token_id, "service_account_id": id, "token": secret, "hint": hint, "expires_at": body.expires_at })),
    )
        .into_response())
}

async fn revoke_token(State(st): State<Shared>, actor: Actor, Path(token): Path<String>) -> ApiResult<Response> {
    let row = st.store.service_token_row(&token).await?.ok_or_else(|| ApiError::not_found("no such token"))?;
    managed(&st, &actor, &row.service_account_id).await.map_err(|_| ApiError::not_found("no such token"))?;
    let mut tx = st.store.begin().await?;
    let revoked = service_tokens_revoke(&mut tx, &row.service_account_id, Some(&token)).await?;
    let mut seq = None;
    if !revoked.is_empty() {
        seq = Some(
            changes::record(
                &mut tx,
                "credential.revoked",
                None,
                Some(&row.service_account_id),
                json!({ "token_id": token, "sub": row.service_account_id }),
            )
            .await?,
        );
    }
    tx.commit().await?;
    st.store
        .audit(
            &actor.audit("service_token_revoked").subject(&row.service_account_id).detail(json!({ "token_id": token })),
        )
        .await?;
    Ok(Json(json!({ "seq": seq })).into_response())
}
