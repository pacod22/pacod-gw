//! Deleting an account: everything stored about the person goes. Their
//! sessions end first (clients hear of it by back-channel), then the account and every row about
//! it; in records of what they did to other objects, they become `deleted account`. The feed
//! carries `principal.deleted`; a tombstone stays until every product has confirmed it deleted
//! its own data.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use pacod_id_authz::Subject;
use serde::Deserialize;
use serde_json::json;
use sqlx::Row;

use super::actor::Actor;
use crate::authz::bindings::{self, Protect};
use crate::error::{ApiError, ApiResult};
use crate::http::Shared;
use crate::http::caller::CallerClient;
use crate::store::authz as db;
use crate::store::{Account, AuditEntry, changes};

/// What replaces a deleted person where a record of their actions stays.
pub const DELETED: &str = "deleted account";

pub fn internal_routes() -> Router<Shared> {
    Router::new().route("/internal/v1/deletions/{sub}/done", post(done))
}

/// `DELETE /admin/v1/accounts/{id}`.
pub async fn delete(State(st): State<Shared>, actor: Actor, Path(id): Path<String>) -> ApiResult<Response> {
    let account = super::accounts::managed(&st, &actor, &id).await?;
    let seq = delete_account(&st, &account, &actor.subject, &actor.id).await?;
    let by = if actor.id == id { DELETED } else { actor.id.as_str() };
    st.store
        .audit(&AuditEntry {
            realm: Some(actor.realm.clone()),
            client_id: actor.client.clone(),
            ..AuditEntry::new("account_deleted").by(by, actor.kind.as_str())
        })
        .await?;
    Ok(Json(json!({ "seq": seq })).into_response())
}

/// Deletes an account for `actor` (whose rights may let a protected role's last holder go).
/// Refused with 409 `last_holder` while the account is the last holder of a protected role.
pub async fn delete_account(st: &Shared, account: &Account, actor: &Subject, by: &str) -> ApiResult<i64> {
    let catalog = st.catalog.get();
    let id = account.id.as_str();
    {
        let mut tx = st.store.begin().await?;
        db::lock_head(&mut tx).await?;
        bindings::check_leaving(&mut tx, &catalog, actor, id).await?;
    }
    crate::oidc::logout::end_all(st, id, "account_deleted").await?;

    let mut tx = st.store.begin().await?;
    db::lock_head(&mut tx).await?;
    bindings::check_leaving(&mut tx, &catalog, actor, id).await?;
    let mut external = serde_json::Map::new();
    for r in sqlx::query("SELECT source, external_id FROM account_external_ids WHERE account_id = $1")
        .bind(id)
        .fetch_all(&mut *tx)
        .await?
    {
        external.insert(r.try_get("source")?, json!(r.try_get::<String, _>("external_id")?));
    }
    let held = db::bindings_of(&mut tx, id).await?;
    bindings::apply(&mut tx, &catalog, Protect::Skip, &held, &[]).await?;
    let email = account.email.as_str();
    for (q, value) in [
        ("DELETE FROM invitations WHERE email = $1", email),
        ("UPDATE invitations SET invited_by = 'deleted account' WHERE invited_by = $1", id),
        ("UPDATE role_bindings SET created_by = 'deleted account' WHERE created_by = $1", id),
        ("UPDATE custom_roles SET created_by = 'deleted account' WHERE created_by = $1", id),
        ("UPDATE service_accounts SET created_by = 'deleted account' WHERE created_by = $1", id),
        ("DELETE FROM mail_outbox WHERE to_address = $1", email),
        ("DELETE FROM throttle WHERE key = $1", email),
        ("DELETE FROM throttle WHERE key LIKE '%:' || $1", id),
        ("DELETE FROM audit_log WHERE subject = $1", id),
        ("DELETE FROM audit_log WHERE subject = $1", email),
        ("UPDATE audit_log SET actor = 'deleted account', ip = '', user_agent = '' WHERE actor = $1", id),
        // Sessions, grants, codes, flows, links, devices, consents, federated identities and
        // external ids go with the account (ON DELETE CASCADE).
        ("DELETE FROM accounts WHERE id = $1", id),
    ] {
        sqlx::query(q).bind(value).execute(&mut *tx).await?;
    }
    let pending: Vec<String> =
        sqlx::query_scalar("SELECT product FROM catalog ORDER BY product").fetch_all(&mut *tx).await?;
    if !pending.is_empty() {
        sqlx::query(
            "INSERT INTO account_deletions (account_id, requested_by, pending_products, external_ids) VALUES ($1, $2, $3, $4)
             ON CONFLICT (account_id) DO UPDATE SET pending_products = $3",
        )
        .bind(id)
        .bind(if by == id { DELETED } else { by })
        .bind(&pending)
        .bind(serde_json::Value::Object(external.clone()))
        .execute(&mut *tx)
        .await?;
    }
    let seq = changes::record(
        &mut tx,
        "principal.deleted",
        None,
        Some(id),
        json!({ "sub": id, "principal_kind": "user", "external_ids": external }),
    )
    .await?;
    tx.commit().await?;
    tracing::info!(products = ?pending, "an account was deleted");
    Ok(seq)
}

#[derive(Deserialize)]
struct DoneQuery {
    product: String,
}

/// A product has deleted its data about a deleted account.
async fn done(
    State(st): State<Shared>,
    caller: CallerClient,
    Path(sub): Path<String>,
    Query(q): Query<DoneQuery>,
) -> ApiResult<StatusCode> {
    if !caller.is_service() || caller.product.as_deref() != Some(q.product.as_str()) {
        return Err(ApiError::forbidden("a product's service confirms for its own product")
            .with_details(json!({ "code": "wrong_product" })));
    }
    let mut tx = st.store.begin().await?;
    let left: Option<Vec<String>> = sqlx::query_scalar(
        "UPDATE account_deletions SET pending_products = array_remove(pending_products, $2) WHERE account_id = $1
         RETURNING pending_products",
    )
    .bind(&sub)
    .bind(&q.product)
    .fetch_optional(&mut *tx)
    .await?;
    if left.is_some_and(|l| l.is_empty()) {
        sqlx::query("DELETE FROM account_deletions WHERE account_id = $1").bind(&sub).execute(&mut *tx).await?;
    }
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
