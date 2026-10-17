//! The admin API, on the internal listener: products' facades call it with a
//! token they exchanged from their caller's, so every decision is made on the caller's own
//! bindings. The hosted UI's admin pages (stage 8) reuse the same handlers.

pub mod accounts;
pub mod actor;
pub mod audit;
pub mod deletion;
pub mod invitations;
pub mod members;
pub mod roles;
pub mod service;

use axum::Router;
use pacod_id_authz::ScopeRef;
use serde_json::json;
use sqlx::PgConnection;

use crate::authz::bindings::ChangeError;
use crate::error::{ApiError, ApiResult};
use crate::http::Shared;
use crate::store::authz as db;

/// The admin API's routes, relative: mounted at `/admin/v1` on the internal listener (tokens
/// products exchanged) and at `/ui-api/v1/admin` on the public one (the hosted admin pages,
/// with the session cookie only).
fn api() -> Router<Shared> {
    Router::new()
        .merge(accounts::routes())
        .merge(service::routes())
        .merge(members::routes())
        .merge(invitations::admin_routes())
        .merge(roles::routes())
        .merge(audit::routes())
}

/// `/admin/v1/*` and the deletion confirmations of `/internal/v1`.
pub fn internal_routes() -> Router<Shared> {
    Router::new().nest("/admin/v1", api()).merge(deletion::internal_routes())
}

/// The hosted UI's invitation pages and its admin pages.
pub fn public_routes() -> Router<Shared> {
    invitations::public_routes().nest("/ui-api/v1/admin", api().layer(axum::middleware::from_fn(actor::through_cookie)))
}

impl From<ChangeError> for ApiError {
    fn from(e: ChangeError) -> Self {
        let message = e.to_string();
        match e {
            ChangeError::ScopeUnknown(_) => {
                ApiError::conflict(message).with_details(json!({ "code": "scope_unknown" }))
            }
            ChangeError::PrincipalUnknown(_) => {
                ApiError::not_found(message).with_details(json!({ "code": "principal_unknown" }))
            }
            ChangeError::LastHolder(v) => ApiError::conflict(message)
                .with_details(json!({ "code": "last_holder", "role": v.role, "scope": v.scope.to_string() })),
            ChangeError::Store(s) => s.into(),
        }
    }
}

/// A scope written as `platform`, `organisation:o1` or `gw.workspace:w1`.
pub fn parse_scope(s: &str) -> ApiResult<ScopeRef> {
    s.parse()
        .map_err(|_| ApiError::invalid(format!("{s:?} is not a scope")).with_details(json!({ "code": "bad_scope" })))
}

/// The path of a registered scope; 409 `scope_unknown` otherwise.
pub async fn path_of(conn: &mut PgConnection, scope: &ScopeRef) -> ApiResult<Vec<ScopeRef>> {
    db::path_of(conn, scope).await?.ok_or_else(|| {
        ApiError::conflict(format!("the scope {scope} is not registered"))
            .with_details(json!({ "code": "scope_unknown" }))
    })
}

/// The products a scope concerns: its own product, or for an organisation the products
/// registered with it. pacod-id's own roles concern every scope.
pub async fn products_at(conn: &mut PgConnection, scope: &ScopeRef) -> ApiResult<Vec<String>> {
    let mut products = vec!["id".to_owned()];
    match scope.kind.split_once('.') {
        Some((p, _)) => products.push(p.to_owned()),
        None if scope.kind == pacod_id_authz::ORGANISATION => {
            if let Some(o) = db::organisation(conn, scope.id.as_deref().unwrap_or("")).await? {
                products.extend(o.products);
            }
        }
        None => {
            let rows: Vec<String> = sqlx::query_scalar("SELECT product FROM catalog").fetch_all(&mut *conn).await?;
            products.extend(rows);
        }
    }
    Ok(products)
}

/// A page size from a query: `default` when absent, at most 500.
pub fn page(limit: Option<i64>, default: i64) -> i64 {
    limit.unwrap_or(default).clamp(1, 500)
}
