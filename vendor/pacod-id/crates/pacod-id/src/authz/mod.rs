//! Authorization: the catalogue of products' manifests, organisations and product
//! scopes, role bindings, and the feed that carries their changes to the products.

pub mod bindings;
pub mod bootstrap;
pub mod catalog;
pub mod feed;
pub mod internal;
pub mod scopes;

use std::sync::{Arc, LazyLock, RwLock};

use pacod_id_authz::{Catalog, Manifest, ScopeRef};

use crate::store::{Store, StoreError};

/// pacod-id's own manifest: `id.admin`, `id.auditor`.
pub const ID_MANIFEST: &str = include_str!("id.yaml");

static ID: LazyLock<Manifest> = LazyLock::new(|| Manifest::from_yaml(ID_MANIFEST).expect("the id manifest reads"));

/// The catalogue every check uses: the built-in `id` manifest and the products' pushed ones.
/// Each instance reloads it after a push and every few seconds.
pub struct CatalogCell {
    inner: RwLock<Arc<Catalog>>,
}

impl Default for CatalogCell {
    fn default() -> Self {
        CatalogCell { inner: RwLock::new(Arc::new(Catalog::new().with(ID.clone()))) }
    }
}

/// The catalogue with pacod-id's own manifest only, to which the products' are added.
pub fn base_catalog() -> Catalog {
    Catalog::new().with(ID.clone())
}

impl CatalogCell {
    pub fn get(&self) -> Arc<Catalog> {
        self.inner.read().expect("the catalogue lock").clone()
    }

    /// Reads the pushed manifests again. One that no longer reads (written by a newer version)
    /// is left out and logged, so the others keep working.
    pub async fn reload(&self, store: &Store) -> Result<(), StoreError> {
        let mut catalog = base_catalog();
        for row in store.catalog_rows().await? {
            match serde_json::from_value(row.manifest)
                .map_err(|e| e.to_string())
                .and_then(|source| Manifest::from_source(source).map_err(|e| e.to_string()))
            {
                Ok(m) => catalog.insert(m),
                Err(e) => tracing::error!(product = %row.product, error = %e, "a stored manifest does not read"),
            }
        }
        *self.inner.write().expect("the catalogue lock") = Arc::new(catalog);
        Ok(())
    }
}

/// A path as the feed and the API write it: `["platform", "organisation:o1", "gw.workspace:w1"]`.
pub fn path_json(path: &[ScopeRef]) -> serde_json::Value {
    serde_json::Value::Array(path.iter().map(|s| serde_json::Value::String(s.to_string())).collect())
}
