//! The rules engine of pacod-id.
//!
//! Each product declares its roles, permissions and scope types in a manifest. Bindings say who
//! holds which role where, with the scope's full path from the platform down. Evaluation is a
//! pure function of the catalogue of manifests and the bindings; the same semantics are ported
//! to TypeScript (`@pacod/id-authz`) and Python (`pacod_id.authz`), and the shared vectors in
//! `policy/engine-vectors/` keep the three in step.

mod eval;
mod manifest;
mod scope;
#[cfg(feature = "vectors")]
pub mod vectors;

pub use eval::{Binding, Catalog, GrantError, HeldBinding, Subject, SubjectKind, Violation};
pub use manifest::{
    CustomRolesSource, Manifest, ManifestError, ManifestSource, ProtectedSource, Protection, Role, RoleSource,
    ScopeTypeSource,
};
pub use scope::{BadScope, ORGANISATION, PLATFORM, ScopeRef};

/// Reads YAML the way every port of the engine does: YAML 1.2 booleans only (`true`, `false`;
/// not `yes` or `on`), and a key twice in one map is an error.
#[cfg(feature = "yaml")]
pub(crate) fn read_yaml<T: serde::de::DeserializeOwned>(text: &str) -> Result<T, String> {
    let options = serde_saphyr::options! { strict_booleans: true };
    serde_saphyr::from_str_with_options(text, options).map_err(|e| e.to_string())
}
