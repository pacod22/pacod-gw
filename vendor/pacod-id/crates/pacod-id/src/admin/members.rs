//! Role bindings and a scope's members (the gateway's `/role-bindings` and
//! members pages). Giving or taking a role needs the role's grant permission at the scope (the
//! manifest's `grant`), or `id.bindings.manage`; no change leaves a protected role's scope
//! without a holder unless the manifest's bypass lets the actor.

use std::collections::BTreeMap;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, put};
use axum::{Json, Router};
use pacod_id_authz::ScopeRef;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;

use super::actor::{Actor, not_allowed};
use crate::authz::bindings::{self, NewBinding, Protect};
use crate::authz::path_json;
use crate::error::{ApiError, ApiResult};
use crate::http::Shared;
use crate::store::authz::{self as db, BindingRow};

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/bindings", get(list).post(create))
        .route("/bindings/{id}", delete(remove))
        .route("/scopes/{kind}/{id}/members", get(members))
        .route("/scopes/{kind}/{id}/members/{principal}", put(set_member).delete(remove_member))
}

pub fn view(b: &BindingRow, path: &[ScopeRef]) -> Value {
    json!({
        "id": b.id,
        "principal": b.principal_id,
        "principal_kind": b.principal_kind,
        "role": b.role,
        "scope": b.scope().to_string(),
        "path": path_json(path),
        "source": b.source,
        "created_by": b.created_by,
        "created_at": b.created_at,
    })
}

#[derive(Deserialize)]
struct ListQuery {
    principal: Option<String>,
    scope: Option<String>,
}

/// Bindings of a principal or at a scope: the actor's own, and those at scopes whose members
/// the actor sees.
async fn list(State(st): State<Shared>, actor: Actor, Query(q): Query<ListQuery>) -> ApiResult<Response> {
    let catalog = st.catalog.get();
    let mut conn = st.store.pool().acquire().await?;
    let rows = match (&q.principal, &q.scope) {
        (_, Some(scope)) => {
            let scope = super::parse_scope(scope)?;
            db::bindings_at(&mut conn, std::slice::from_ref(&scope), false).await?
        }
        (Some(p), None) => db::bindings_of(&mut conn, p).await?,
        (None, None) => db::bindings_of(&mut conn, &actor.id).await?,
    };
    let rows: Vec<BindingRow> =
        rows.into_iter().filter(|b| q.principal.as_ref().is_none_or(|p| *p == b.principal_id)).collect();
    let mut out = Vec::new();
    for b in rows {
        let Some(path) = db::path_of(&mut conn, &b.scope()).await? else { continue };
        let products = super::products_at(&mut conn, &b.scope()).await?;
        if actor.owns(&b) || actor.sees_members(&catalog, &path, &products) {
            out.push(view(&b, &path));
        }
    }
    Ok(Json(json!({ "bindings": out })).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewBindingInput {
    principal: String,
    role: String,
    scope: String,
}

/// Adds one binding, keeping the principal's others.
async fn create(State(st): State<Shared>, actor: Actor, Json(body): Json<NewBindingInput>) -> ApiResult<Response> {
    let catalog = st.catalog.get();
    let scope = super::parse_scope(&body.scope)?;
    let mut tx = st.store.begin().await?;
    db::lock_head(&mut tx).await?;
    let path = super::path_of(&mut tx, &scope).await?;
    actor.may_grant(&catalog, &body.role, &path)?;
    let add = [NewBinding {
        principal: body.principal.clone(),
        role: body.role.clone(),
        scope: scope.clone(),
        source: "manual",
        created_by: actor.id.clone(),
    }];
    let applied = bindings::apply(&mut tx, &catalog, Protect::As(&actor.subject), &[], &add).await?;
    let Some(id) = applied.added.first() else {
        return Err(ApiError::conflict("the principal holds this role there already")
            .with_details(json!({ "code": "binding_exists" })));
    };
    let row = db::binding(&mut tx, id).await?.expect("just added");
    tx.commit().await?;
    st.store
        .audit(
            &actor
                .audit("binding_added")
                .subject(&body.principal)
                .detail(json!({ "role": body.role, "scope": scope.to_string() })),
        )
        .await?;
    let mut out = view(&row, &path);
    out["seq"] = json!(applied.seq);
    Ok((StatusCode::CREATED, Json(out)).into_response())
}

async fn remove(State(st): State<Shared>, actor: Actor, Path(id): Path<String>) -> ApiResult<Response> {
    let catalog = st.catalog.get();
    let mut tx = st.store.begin().await?;
    db::lock_head(&mut tx).await?;
    let row = db::binding(&mut tx, &id).await?.ok_or_else(|| ApiError::not_found("no such binding"))?;
    let path = db::path_of(&mut tx, &row.scope()).await?.unwrap_or_default();
    actor.may_grant(&catalog, &row.role, &path)?;
    let applied =
        bindings::apply(&mut tx, &catalog, Protect::As(&actor.subject), std::slice::from_ref(&row), &[]).await?;
    tx.commit().await?;
    st.store
        .audit(
            &actor
                .audit("binding_removed")
                .subject(&row.principal_id)
                .detail(json!({ "role": row.role, "scope": row.scope().to_string() })),
        )
        .await?;
    Ok(Json(json!({ "seq": applied.seq })).into_response())
}

fn scope_from(kind: &str, id: &str) -> ScopeRef {
    if kind == pacod_id_authz::PLATFORM { ScopeRef::platform() } else { ScopeRef::of(kind, id) }
}

/// The product a role belongs to.
fn product(role: &str) -> &str {
    role.split_once('.').map_or("", |(p, _)| p)
}

/// The members of a scope: everyone with a binding at it or below it, and, marked inherited,
/// the bindings at the scopes above it (not the platform's). The acting product's roles only,
/// when the call comes through a product.
async fn members(
    State(st): State<Shared>,
    actor: Actor,
    Path((kind, id)): Path<(String, String)>,
) -> ApiResult<Response> {
    let catalog = st.catalog.get();
    let scope = scope_from(&kind, &id);
    let mut conn = st.store.pool().acquire().await?;
    let path = super::path_of(&mut conn, &scope).await?;
    let products = super::products_at(&mut conn, &scope).await?;
    if !actor.sees_members(&catalog, &path, &products) {
        return Err(not_allowed());
    }
    let below = db::subtree(&mut conn, &scope).await?;
    let above: Vec<ScopeRef> = path[..path.len() - 1].iter().filter(|s| !s.is_platform()).cloned().collect();
    let mut rows: Vec<(BindingRow, bool)> =
        db::bindings_at(&mut conn, &below, false).await?.into_iter().map(|b| (b, false)).collect();
    rows.extend(db::bindings_at(&mut conn, &above, false).await?.into_iter().map(|b| (b, true)));
    if let Some(p) = &actor.product {
        rows.retain(|(b, _)| product(&b.role) == p);
    }
    let ids: Vec<String> = rows.iter().map(|(b, _)| b.principal_id.clone()).collect();
    let mut who: BTreeMap<String, Value> = BTreeMap::new();
    for r in sqlx::query("SELECT id, email, display_name, status FROM accounts WHERE id = ANY($1)")
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?
    {
        let id: String = r.try_get("id")?;
        who.insert(
            id.clone(),
            json!({ "principal": id, "principal_kind": "user", "email": r.try_get::<String, _>("email")?,
                    "name": r.try_get::<String, _>("display_name")?, "status": r.try_get::<String, _>("status")?, "roles": [] }),
        );
    }
    for r in sqlx::query("SELECT id, name, status FROM service_accounts WHERE id = ANY($1)")
        .bind(&ids)
        .fetch_all(&mut *conn)
        .await?
    {
        let id: String = r.try_get("id")?;
        who.insert(
            id.clone(),
            json!({ "principal": id, "principal_kind": "service", "email": null,
                    "name": r.try_get::<String, _>("name")?, "status": r.try_get::<String, _>("status")?, "roles": [] }),
        );
    }
    for (b, inherited) in &rows {
        if let Some(m) = who.get_mut(&b.principal_id) {
            m["roles"].as_array_mut().expect("an array").push(json!({
                "binding": b.id, "role": b.role, "scope": b.scope().to_string(), "inherited": inherited,
            }));
        }
    }
    let mut members: Vec<Value> = who.into_values().collect();
    members
        .sort_by_key(|m| (m["email"].as_str().unwrap_or("~").to_owned(), m["name"].as_str().unwrap_or("").to_owned()));
    Ok(Json(json!({ "scope": scope.to_string(), "members": members })).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetMember {
    /// The one role to hold at the scope, or null for none.
    role: Option<String>,
}

/// Gives a principal one role at exactly this scope, replacing the roles of the same product
/// it held there (the gateway's members page); roles below the scope stay.
async fn set_member(
    State(st): State<Shared>,
    actor: Actor,
    Path((kind, id, principal)): Path<(String, String, String)>,
    Json(body): Json<SetMember>,
) -> ApiResult<Response> {
    let catalog = st.catalog.get();
    let scope = scope_from(&kind, &id);
    let of_product = body.role.as_deref().map(product).map(str::to_owned).or_else(|| actor.product.clone());
    let mut tx = st.store.begin().await?;
    db::lock_head(&mut tx).await?;
    let path = super::path_of(&mut tx, &scope).await?;
    let held: Vec<BindingRow> = db::bindings_at(&mut tx, std::slice::from_ref(&scope), false)
        .await?
        .into_iter()
        .filter(|b| b.principal_id == principal && of_product.as_deref().is_none_or(|p| product(&b.role) == p))
        .collect();
    // The rights first: the "nothing to change" answer must not tell who holds what.
    if let Some(role) = &body.role {
        actor.may_grant(&catalog, role, &path)?;
    }
    for b in &held {
        actor.may_grant(&catalog, &b.role, &path)?;
    }
    if let Some(role) = &body.role
        && held.len() == 1
        && held[0].role == *role
    {
        return Ok(Json(json!({ "seq": null })).into_response());
    }
    let mut add = Vec::new();
    if let Some(role) = &body.role {
        add.push(NewBinding {
            principal: principal.clone(),
            role: role.clone(),
            scope: scope.clone(),
            source: "manual",
            created_by: actor.id.clone(),
        });
    }
    let applied = bindings::apply(&mut tx, &catalog, Protect::As(&actor.subject), &held, &add).await?;
    tx.commit().await?;
    st.store
        .audit(
            &actor
                .audit("member_role_set")
                .subject(&principal)
                .detail(json!({ "role": body.role, "scope": scope.to_string() })),
        )
        .await?;
    Ok(Json(json!({ "seq": applied.seq })).into_response())
}

#[derive(Deserialize)]
struct RemoveQuery {
    /// Also the roles below the scope (the default: leaving an organisation leaves its workspaces).
    below: Option<bool>,
}

/// Takes a principal's roles at the scope away, and below it unless `below=false`.
async fn remove_member(
    State(st): State<Shared>,
    actor: Actor,
    Path((kind, id, principal)): Path<(String, String, String)>,
    Query(q): Query<RemoveQuery>,
) -> ApiResult<Response> {
    let catalog = st.catalog.get();
    let scope = scope_from(&kind, &id);
    let mut tx = st.store.begin().await?;
    db::lock_head(&mut tx).await?;
    super::path_of(&mut tx, &scope).await?;
    let scopes = if q.below.unwrap_or(true) { db::subtree(&mut tx, &scope).await? } else { vec![scope.clone()] };
    let held: Vec<BindingRow> = db::bindings_at(&mut tx, &scopes, false)
        .await?
        .into_iter()
        .filter(|b| b.principal_id == principal && actor.product.as_deref().is_none_or(|p| product(&b.role) == p))
        .collect();
    if held.is_empty() {
        return Err(
            ApiError::not_found("the principal holds no role here").with_details(json!({ "code": "not_a_member" }))
        );
    }
    for b in &held {
        let path = db::path_of(&mut tx, &b.scope()).await?.unwrap_or_default();
        actor.may_grant(&catalog, &b.role, &path)?;
    }
    let applied = bindings::apply(&mut tx, &catalog, Protect::As(&actor.subject), &held, &[]).await?;
    tx.commit().await?;
    st.store
        .audit(&actor.audit("member_removed").subject(&principal).detail(json!({ "scope": scope.to_string() })))
        .await?;
    Ok(Json(json!({ "seq": applied.seq })).into_response())
}
