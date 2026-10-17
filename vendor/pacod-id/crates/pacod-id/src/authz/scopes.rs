//! Organisations and product scopes. Products register them with their own
//! ids, after their own transaction commits; every call is idempotent, so a product's outbox can
//! send it again. Deleting a scope takes the bindings and invitations of everything below it.

use pacod_id_authz::{ORGANISATION, ScopeRef};
use serde_json::json;
use sqlx::PgConnection;

use super::bindings::{self, ChangeError, Protect};
use super::path_json;
use crate::http::Shared;
use crate::store::authz::{self as db, ScopeRow};
use crate::store::{StoreError, changes};

#[derive(Debug, thiserror::Error)]
pub enum ScopeError {
    #[error("{0:?} is not a usable id")]
    BadId(String),
    #[error("the product {0} has not pushed its catalogue")]
    CatalogMissing(String),
    #[error("{kind} is not a scope type of {product}")]
    NotYourType { product: String, kind: String },
    #[error("a {kind} belongs to a {expected}")]
    BadParent { kind: String, expected: String },
    #[error("the parent {0} is not registered")]
    ParentUnknown(String),
    #[error("{0} is registered under another parent")]
    ParentChanged(String),
    #[error("another organisation has the slug {0:?}")]
    SlugTaken(String),
    #[error("{0} is built into a product and is not deleted")]
    System(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<sqlx::Error> for ScopeError {
    fn from(e: sqlx::Error) -> Self {
        ScopeError::Store(e.into())
    }
}

impl From<ChangeError> for ScopeError {
    fn from(e: ChangeError) -> Self {
        match e {
            ChangeError::Store(s) => ScopeError::Store(s),
            other => ScopeError::Store(StoreError::Corrupt(other.to_string())),
        }
    }
}

/// Ids are the products' own (`org_…`, `ws_…`): no separator, no spaces.
fn check_id(id: &str) -> Result<(), ScopeError> {
    let ok = !id.is_empty() && id.len() <= 100 && !id.contains([':', '/']) && !id.contains(char::is_whitespace);
    if ok { Ok(()) } else { Err(ScopeError::BadId(id.to_owned())) }
}

/// The product a scope type belongs to; `None` for organisations, which are shared.
fn product_of(kind: &str) -> Option<&str> {
    kind.split_once('.').map(|(p, _)| p)
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrganisationInput {
    /// Unique; the id when left out.
    #[serde(default)]
    pub slug: Option<String>,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub system: bool,
}

/// Creates or renames an organisation and registers `product` with it. The feed's version
/// afterwards.
pub async fn put_organisation(
    st: &Shared,
    product: &str,
    id: &str,
    input: &OrganisationInput,
) -> Result<i64, ScopeError> {
    check_id(id)?;
    let slug = input.slug.clone().unwrap_or_else(|| id.to_owned());
    let mut tx = st.store.begin().await?;
    let head = db::lock_head(&mut tx).await?;
    let before = db::organisation(&mut tx, id).await?;
    match db::organisation_upsert(&mut tx, id, &slug, &input.display_name, input.system, product).await {
        Err(StoreError::Conflict(_)) => return Err(ScopeError::SlugTaken(slug)),
        r => r?,
    }
    let after = db::organisation(&mut tx, id).await?.ok_or(StoreError::NotFound)?;
    let seq = if before.as_ref() == Some(&after) { head } else { organisation_upserted(&mut tx, &after).await? };
    tx.commit().await?;
    Ok(seq)
}

/// The feed's event for an organisation as it now is: the snapshot's fields, in the same shape.
pub(crate) async fn organisation_upserted(
    tx: &mut sqlx::PgConnection,
    org: &db::OrganisationRow,
) -> Result<i64, StoreError> {
    changes::record(
        tx,
        "scope.upserted",
        None,
        None,
        json!({
            "type": ORGANISATION, "id": org.id, "slug": org.slug, "display_name": org.display_name,
            "status": org.status, "system": org.system, "products": org.products,
            "path": path_json(&[ScopeRef::platform(), ScopeRef::organisation(&org.id)]),
        }),
    )
    .await
}

/// Unregisters `product` from an organisation; the organisation goes with everything below it
/// once no product has it.
pub async fn delete_organisation(st: &Shared, product: &str, id: &str) -> Result<i64, ScopeError> {
    let mut tx = st.store.begin().await?;
    let head = db::lock_head(&mut tx).await?;
    let Some(org) = db::organisation(&mut tx, id).await? else { return Ok(head) };
    if org.system {
        return Err(ScopeError::System(format!("organisation:{id}")));
    }
    let remaining = db::organisation_unregister(&mut tx, id, product).await?;
    let seq = if remaining.is_empty() {
        delete_subtree(&mut tx, st, &ScopeRef::organisation(id)).await?.unwrap_or(head)
    } else if remaining != org.products {
        // Still another product's. What the leaving product put there goes with it: its scopes
        // below the organisation (with their bindings and invitations), and its roles and
        // invitations at the organisation itself. The others' stay.
        let root = ScopeRef::organisation(id);
        for scope in db::subtree(&mut tx, &root).await?.into_iter().filter(|s| product_of(&s.kind) == Some(product)) {
            delete_subtree(&mut tx, st, &scope).await?;
        }
        let prefix = format!("{product}.");
        let held: Vec<_> = db::bindings_at(&mut tx, std::slice::from_ref(&root), false)
            .await?
            .into_iter()
            .filter(|b| b.role.starts_with(&prefix))
            .collect();
        bindings::apply(&mut tx, &st.catalog.get(), Protect::Skip, &held, &[]).await?;
        db::invitations_delete_of_product(&mut tx, &root, product).await?;
        // Its list of products changed, as the snapshot shows.
        let now = db::organisation(&mut tx, id).await?.ok_or(StoreError::NotFound)?;
        organisation_upserted(&mut tx, &now).await?
    } else {
        head
    };
    tx.commit().await?;
    Ok(seq)
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeInput {
    /// `organisation:<id>`, or another scope of the product (`gw.workspace:<id>`).
    pub parent: ScopeRef,
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub system: bool,
}

/// Registers or renames a product scope. Its parent is fixed once registered.
pub async fn put_scope(
    st: &Shared,
    product: &str,
    kind: &str,
    id: &str,
    input: &ScopeInput,
) -> Result<i64, ScopeError> {
    check_id(id)?;
    let catalog = st.catalog.get();
    let manifest = catalog.manifest(product).ok_or_else(|| ScopeError::CatalogMissing(product.to_owned()))?;
    let not_yours = || ScopeError::NotYourType { product: product.to_owned(), kind: kind.to_owned() };
    if product_of(kind) != Some(product) {
        return Err(not_yours());
    }
    let expected = manifest.scope_types().find(|(k, _)| *k == kind).map(|(_, p)| p.to_owned()).ok_or_else(not_yours)?;
    if input.parent.kind != expected || input.parent.id.is_none() && expected != pacod_id_authz::PLATFORM {
        return Err(ScopeError::BadParent { kind: kind.to_owned(), expected });
    }
    let mut tx = st.store.begin().await?;
    let head = db::lock_head(&mut tx).await?;
    let mut path = db::path_of(&mut tx, &input.parent)
        .await?
        .ok_or_else(|| ScopeError::ParentUnknown(input.parent.to_string()))?;
    let row = ScopeRow {
        kind: kind.to_owned(),
        id: id.to_owned(),
        parent: input.parent.clone(),
        product: product.to_owned(),
        display_name: input.display_name.clone(),
        system: input.system,
    };
    let before = db::scope(&mut tx, kind, id).await?;
    if before.as_ref().is_some_and(|b| b.parent != row.parent) {
        return Err(ScopeError::ParentChanged(format!("{kind}:{id}")));
    }
    db::scope_upsert(&mut tx, &row).await?;
    let after = db::scope(&mut tx, kind, id).await?.ok_or(StoreError::NotFound)?;
    let seq = if before.as_ref() == Some(&after) {
        head
    } else {
        path.push(ScopeRef::of(kind, id));
        changes::record(
            &mut tx,
            "scope.upserted",
            Some(product),
            None,
            json!({
                "type": kind, "id": id, "parent": after.parent.to_string(), "display_name": after.display_name,
                "system": after.system, "path": path_json(&path),
            }),
        )
        .await?
    };
    tx.commit().await?;
    Ok(seq)
}

/// Deletes a product scope with everything below it.
pub async fn delete_scope(st: &Shared, product: &str, kind: &str, id: &str) -> Result<i64, ScopeError> {
    if product_of(kind) != Some(product) {
        return Err(ScopeError::NotYourType { product: product.to_owned(), kind: kind.to_owned() });
    }
    let mut tx = st.store.begin().await?;
    let head = db::lock_head(&mut tx).await?;
    let Some(scope) = db::scope(&mut tx, kind, id).await? else { return Ok(head) };
    if scope.system {
        return Err(ScopeError::System(format!("{kind}:{id}")));
    }
    let seq = delete_subtree(&mut tx, st, &ScopeRef::of(kind, id)).await?.unwrap_or(head);
    tx.commit().await?;
    Ok(seq)
}

/// Deletes a scope, the scopes below it, their bindings and invitations. The version of the
/// last event.
async fn delete_subtree(conn: &mut PgConnection, st: &Shared, root: &ScopeRef) -> Result<Option<i64>, ScopeError> {
    let all = db::subtree(conn, root).await?;
    let mut paths = Vec::new();
    for s in &all {
        paths.push(db::path_of(conn, s).await?.unwrap_or_default());
    }
    let held = db::bindings_at(conn, &all, false).await?;
    let mut seq = bindings::apply(conn, &st.catalog.get(), Protect::Skip, &held, &[]).await?.seq;
    db::scopes_delete(conn, &all).await?;
    for (scope, path) in all.iter().zip(paths) {
        let product = product_of(&scope.kind);
        seq = Some(
            changes::record(
                conn,
                "scope.deleted",
                product,
                None,
                json!({ "type": scope.kind, "id": scope.id, "path": path_json(&path) }),
            )
            .await?,
        );
    }
    Ok(seq)
}
