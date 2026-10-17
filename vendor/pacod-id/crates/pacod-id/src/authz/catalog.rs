//! Products push their manifests at start: `PUT /internal/v1/catalog/{product}`.
//! The same manifest again changes nothing. A manifest that drops a role or narrows where it
//! may be bound is refused while bindings use it, unless the push says what becomes of them.

use std::collections::BTreeMap;

use pacod_id_authz::{Manifest, ManifestError, ManifestSource};
use serde::Deserialize;
use serde_json::json;

use super::bindings::{self, ChangeError, NewBinding, Protect};
use crate::http::Shared;
use crate::store::authz::{self as db, BindingRow};
use crate::store::{StoreError, changes};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Push {
    pub manifest: ManifestSource,
    /// For bindings of roles the new manifest drops or no longer allows at their scope: the
    /// role they become, or null to remove them.
    #[serde(default)]
    pub migrate: BTreeMap<String, Option<String>>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Pushed {
    pub product: String,
    pub version: i64,
    pub changed: bool,
    pub seq: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum PushError {
    #[error(transparent)]
    Manifest(#[from] ManifestError),
    #[error("the manifest is for {0}")]
    WrongProduct(String),
    #[error("the id manifest is built into pacod-id")]
    Reserved,
    #[error("bindings still use {0:?}; say in `migrate` what becomes of them")]
    RoleInUse(Vec<String>),
    #[error("scopes of {0:?} are registered; delete them first")]
    ScopeTypeInUse(Vec<String>),
    #[error("custom roles exist; the manifest must keep allowing them")]
    CustomRolesInUse,
    #[error("migrate: {0}")]
    BadMigration(String),
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl From<sqlx::Error> for PushError {
    fn from(e: sqlx::Error) -> Self {
        PushError::Store(e.into())
    }
}

impl From<ChangeError> for PushError {
    fn from(e: ChangeError) -> Self {
        match e {
            ChangeError::Store(s) => PushError::Store(s),
            other => PushError::BadMigration(other.to_string()),
        }
    }
}

/// Whether a binding counts under a manifest: its role exists (or is a custom role the manifest
/// allows) and may be held at the binding's scope type.
fn fits(m: &Manifest, b: &BindingRow) -> bool {
    let local = b.role.strip_prefix(&format!("{}.", m.product())).unwrap_or("");
    if let Some(name) = local.strip_prefix("custom.") {
        return !name.is_empty() && m.custom_roles_at().is_some_and(|at| at.contains(&b.scope_type));
    }
    m.role(&b.role).is_some_and(|r| r.at.contains(&b.scope_type))
}

pub async fn push(st: &Shared, product: &str, push: Push) -> Result<Pushed, PushError> {
    if product == "id" {
        return Err(PushError::Reserved);
    }
    let source = serde_json::to_value(&push.manifest).map_err(|e| StoreError::Corrupt(e.to_string()))?;
    let manifest = Manifest::from_source(push.manifest)?;
    if manifest.product() != product {
        return Err(PushError::WrongProduct(manifest.product().to_owned()));
    }
    let mut tx = st.store.begin().await?;
    let head = db::lock_head(&mut tx).await?;
    let current = db::catalog_row_for_update(&mut tx, product).await?;
    if let Some(c) = &current
        && c.manifest == source
    {
        drop(tx);
        // Stored already, perhaps by `pacod-id import` from outside this process: this instance
        // takes it now rather than at its next reload.
        let held = st.catalog.get().manifest(product).and_then(|m| serde_json::to_value(m.source()).ok());
        if held.as_ref() != Some(&source) {
            st.catalog.reload(&st.store).await?;
        }
        return Ok(Pushed { product: product.to_owned(), version: c.version, changed: false, seq: head });
    }
    let old = current
        .as_ref()
        .and_then(|c| serde_json::from_value(c.manifest.clone()).ok())
        .and_then(|s| Manifest::from_source(s).ok());

    let kinds: Vec<&str> = manifest.scope_types().map(|(k, _)| k).collect();
    let gone: Vec<String> = db::scope_types_in_use(&mut tx, product)
        .await?
        .into_iter()
        .filter(|(k, _)| !kinds.contains(&k.as_str()))
        .map(|(k, _)| k)
        .collect();
    if !gone.is_empty() {
        return Err(PushError::ScopeTypeInUse(gone));
    }
    if manifest.custom_roles_at().is_none() && db::custom_roles_count(&mut tx, product).await? > 0 {
        return Err(PushError::CustomRolesInUse);
    }

    // Bindings that counted under the old manifest and would not under the new one. Bindings
    // that never counted (a bootstrap binding waiting for its product) are left alone.
    let mut in_use = Vec::new();
    let mut remove = Vec::new();
    let mut add = Vec::new();
    for b in db::bindings_of_product(&mut tx, product).await? {
        if fits(&manifest, &b) || !old.as_ref().is_some_and(|o| fits(o, &b)) {
            continue;
        }
        match push.migrate.get(&b.role) {
            None => in_use.push(b.role.clone()),
            Some(None) => remove.push(b),
            Some(Some(to)) => {
                let moved = BindingRow { role: to.clone(), ..b.clone() };
                if !fits(&manifest, &moved) {
                    return Err(PushError::BadMigration(format!("{to} may not be held at a {}", b.scope_type)));
                }
                add.push(NewBinding {
                    principal: b.principal_id.clone(),
                    role: to.clone(),
                    scope: b.scope(),
                    source: "import",
                    created_by: format!("catalog:{product}"),
                });
                remove.push(b);
            }
        }
    }
    if !in_use.is_empty() {
        in_use.sort();
        in_use.dedup();
        return Err(PushError::RoleInUse(in_use));
    }
    let catalog = st.catalog.get();
    bindings::apply(&mut tx, &catalog, Protect::Skip, &remove, &add).await?;
    let version = db::catalog_put(&mut tx, product, &source).await?;
    let seq = changes::record(
        &mut tx,
        "catalog.updated",
        Some(product),
        None,
        json!({ "product": product, "version": version }),
    )
    .await?;
    tx.commit().await?;
    st.catalog.reload(&st.store).await?;
    tracing::info!(product, version, "a product's catalogue changed");
    Ok(Pushed { product: product.to_owned(), version, changed: true, seq })
}
