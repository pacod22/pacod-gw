//! Custom roles: created by a product's administrators at run time,
//! known as `<product>.custom.<name>`, bound and carried in the feed like any role, granting no
//! permission until a manifest or the product's own checks give them meaning.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch};
use axum::{Json, Router};
use pacod_id_authz::ScopeRef;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::Row;

use super::actor::{Actor, not_allowed};
use crate::authz::bindings::{self, NewBinding, Protect};
use crate::error::{ApiError, ApiResult};
use crate::http::Shared;
use crate::store::authz as db;

pub fn routes() -> Router<Shared> {
    Router::new()
        .route("/custom-roles", get(list).post(create))
        .route("/custom-roles/{product}/{name}", patch(update).delete(remove))
}

/// Whether the actor administers `product` at the platform (or is an identity admin), and the
/// product allows custom roles.
fn manages(st: &Shared, actor: &Actor, product: &str) -> ApiResult<()> {
    let catalog = st.catalog.get();
    let manifest = catalog.manifest(product).ok_or_else(|| ApiError::not_found("no such product"))?;
    if manifest.custom_roles_at().is_none() {
        return Err(ApiError::invalid(format!("{product} has no custom roles"))
            .with_details(json!({ "code": "custom_roles_off" })));
    }
    let platform = [ScopeRef::platform()];
    if actor.allowed(&catalog, "id.bindings.manage", &platform)
        || actor.allowed(&catalog, manifest.grant_default(), &platform)
    {
        Ok(())
    } else {
        Err(not_allowed())
    }
}

fn check_name(name: &str) -> ApiResult<()> {
    let mut chars = name.chars();
    let ok = matches!(chars.next(), Some('a'..='z'))
        && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'))
        && name.len() <= 40;
    if ok {
        Ok(())
    } else {
        Err(ApiError::invalid("a name of lowercase letters, digits and _, starting with a letter")
            .with_details(json!({ "code": "bad_name" })))
    }
}

fn view(product: &str, name: &str, description: &str) -> Value {
    json!({ "product": product, "name": name, "role": format!("{product}.custom.{name}"), "description": description })
}

#[derive(Deserialize)]
struct ListQuery {
    product: String,
}

async fn list(State(st): State<Shared>, actor: Actor, Query(q): Query<ListQuery>) -> ApiResult<Response> {
    manages(&st, &actor, &q.product)?;
    let rows = sqlx::query("SELECT name, description FROM custom_roles WHERE product = $1 ORDER BY name")
        .bind(&q.product)
        .fetch_all(st.store.pool())
        .await?;
    let roles = rows
        .iter()
        .map(|r| Ok(view(&q.product, &r.try_get::<String, _>("name")?, &r.try_get::<String, _>("description")?)))
        .collect::<Result<Vec<_>, sqlx::Error>>()?;
    Ok(Json(json!({ "custom_roles": roles })).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewRole {
    product: String,
    name: String,
    #[serde(default)]
    description: String,
}

async fn create(State(st): State<Shared>, actor: Actor, Json(body): Json<NewRole>) -> ApiResult<Response> {
    manages(&st, &actor, &body.product)?;
    check_name(&body.name)?;
    let r = sqlx::query("INSERT INTO custom_roles (product, name, description, created_by) VALUES ($1, $2, $3, $4) ON CONFLICT DO NOTHING")
        .bind(&body.product)
        .bind(&body.name)
        .bind(&body.description)
        .bind(&actor.id)
        .execute(st.store.pool())
        .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::conflict("the role exists").with_details(json!({ "code": "role_exists" })));
    }
    st.store
        .audit(&actor.audit("custom_role_created").subject(&format!("{}.custom.{}", body.product, body.name)))
        .await?;
    Ok((StatusCode::CREATED, Json(view(&body.product, &body.name, &body.description))).into_response())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Patch {
    name: Option<String>,
    description: Option<String>,
}

/// Renames a custom role (its bindings follow) or changes its description.
async fn update(
    State(st): State<Shared>,
    actor: Actor,
    Path((product, name)): Path<(String, String)>,
    Json(body): Json<Patch>,
) -> ApiResult<Response> {
    manages(&st, &actor, &product)?;
    let mut tx = st.store.begin().await?;
    db::lock_head(&mut tx).await?;
    let current: Option<String> =
        sqlx::query_scalar("SELECT description FROM custom_roles WHERE product = $1 AND name = $2 FOR UPDATE")
            .bind(&product)
            .bind(&name)
            .fetch_optional(&mut *tx)
            .await?;
    let description = current.ok_or_else(|| ApiError::not_found("no such custom role"))?;
    let new_name = body.name.clone().unwrap_or_else(|| name.clone());
    check_name(&new_name)?;
    let new_description = body.description.clone().unwrap_or(description);
    let r = sqlx::query("UPDATE custom_roles SET name = $3, description = $4 WHERE product = $1 AND name = $2")
        .bind(&product)
        .bind(&name)
        .bind(&new_name)
        .bind(&new_description)
        .execute(&mut *tx)
        .await;
    if let Err(sqlx::Error::Database(e)) = &r
        && e.is_unique_violation()
    {
        return Err(ApiError::conflict("a role has that name").with_details(json!({ "code": "role_exists" })));
    }
    r?;
    let mut seq = None;
    if new_name != name {
        let old_role = format!("{product}.custom.{name}");
        let new_role = format!("{product}.custom.{new_name}");
        let held: Vec<_> =
            db::bindings_of_product(&mut tx, &product).await?.into_iter().filter(|b| b.role == old_role).collect();
        let add: Vec<NewBinding> = held
            .iter()
            .map(|b| NewBinding {
                principal: b.principal_id.clone(),
                role: new_role.clone(),
                scope: b.scope(),
                source: "manual",
                created_by: b.created_by.clone(),
            })
            .collect();
        seq = bindings::apply(&mut tx, &st.catalog.get(), Protect::Skip, &held, &add).await?.seq;
    }
    tx.commit().await?;
    st.store
        .audit(
            &actor
                .audit("custom_role_updated")
                .subject(&format!("{product}.custom.{new_name}"))
                .detail(json!({ "was": name })),
        )
        .await?;
    let mut out = view(&product, &new_name, &new_description);
    out["seq"] = json!(seq);
    Ok(Json(out).into_response())
}

/// Deletes a custom role and its bindings.
async fn remove(
    State(st): State<Shared>,
    actor: Actor,
    Path((product, name)): Path<(String, String)>,
) -> ApiResult<Response> {
    manages(&st, &actor, &product)?;
    let role = format!("{product}.custom.{name}");
    let mut tx = st.store.begin().await?;
    db::lock_head(&mut tx).await?;
    let gone = sqlx::query("DELETE FROM custom_roles WHERE product = $1 AND name = $2")
        .bind(&product)
        .bind(&name)
        .execute(&mut *tx)
        .await?;
    if gone.rows_affected() == 0 {
        return Err(ApiError::not_found("no such custom role"));
    }
    let held: Vec<_> =
        db::bindings_of_product(&mut tx, &product).await?.into_iter().filter(|b| b.role == role).collect();
    let seq = bindings::apply(&mut tx, &st.catalog.get(), Protect::Skip, &held, &[]).await?.seq;
    tx.commit().await?;
    st.store.audit(&actor.audit("custom_role_deleted").subject(&role)).await?;
    Ok(Json(json!({ "seq": seq })).into_response())
}
