//! A product's manifest: its scope types, roles and permissions, read and checked once.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::scope::{ORGANISATION, PLATFORM};

/// Names a product may not take.
const RESERVED_PRODUCTS: [&str; 3] = [PLATFORM, ORGANISATION, "custom"];

/// A manifest as written: local names, as in `authz/<product>.yaml`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestSource {
    pub product: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub scope_types: BTreeMap<String, ScopeTypeSource>,
    #[serde(default)]
    pub roles: BTreeMap<String, RoleSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_roles: Option<CustomRolesSource>,
    #[serde(default)]
    pub grant: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopeTypeSource {
    pub parent: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleSource {
    pub at: Vec<String>,
    #[serde(default)]
    pub permissions: Vec<String>,
    #[serde(default, skip_serializing_if = "ProtectedSource::is_off")]
    pub protected: ProtectedSource,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub implicit: bool,
}

/// `protected: true`, or `protected: { bypass: <permission> }`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ProtectedSource {
    Flag(bool),
    Bypass { bypass: String },
}

impl Default for ProtectedSource {
    fn default() -> Self {
        ProtectedSource::Flag(false)
    }
}

impl ProtectedSource {
    fn is_off(&self) -> bool {
        matches!(self, ProtectedSource::Flag(false))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CustomRolesSource {
    pub at: Vec<String>,
}

/// Why a manifest was refused. `code()` is the same in every port of the engine.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ManifestError {
    #[error("the manifest does not have the expected shape: {0}")]
    BadManifest(String),
    #[error("{0:?} is not a valid product name")]
    BadProduct(String),
    #[error("{0:?} is not a valid name")]
    BadName(String),
    #[error("scope type {kind:?} has an unknown parent {parent:?}")]
    UnknownParent { kind: String, parent: String },
    #[error("scope type {0:?} is its own ancestor")]
    ParentCycle(String),
    #[error("role {role:?} names an unknown scope type {kind:?}")]
    UnknownScopeType { role: String, kind: String },
    #[error("{0:?} is not a valid permission")]
    BadPermission(String),
    #[error("implicit role {0:?} must be bound at the platform only")]
    ImplicitNotPlatform(String),
    #[error("grant names an unknown role {0:?}")]
    UnknownGrantRole(String),
    #[error("grant has no default")]
    MissingGrantDefault,
}

impl ManifestError {
    pub fn code(&self) -> &'static str {
        match self {
            ManifestError::BadManifest(_) => "bad_manifest",
            ManifestError::BadProduct(_) => "bad_product",
            ManifestError::BadName(_) => "bad_name",
            ManifestError::UnknownParent { .. } => "unknown_parent",
            ManifestError::ParentCycle(_) => "parent_cycle",
            ManifestError::UnknownScopeType { .. } => "unknown_scope_type",
            ManifestError::BadPermission(_) => "bad_permission",
            ManifestError::ImplicitNotPlatform(_) => "implicit_not_platform",
            ManifestError::UnknownGrantRole(_) => "unknown_grant_role",
            ManifestError::MissingGrantDefault => "missing_grant_default",
        }
    }
}

/// When a protected role's last holder at a scope may still go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Protection {
    /// Never.
    Always,
    /// When the actor is allowed this (qualified) permission at the scope.
    Bypass(String),
}

/// A permission pattern of a role, over the product's local permission names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Pattern {
    All,
    /// `x.*`: the prefix with its dot, `x.`.
    Prefix(String),
    Exact(String),
}

impl Pattern {
    pub(crate) fn matches(&self, local: &str) -> bool {
        match self {
            Pattern::All => true,
            Pattern::Prefix(p) => local.len() > p.len() && local.starts_with(p.as_str()),
            Pattern::Exact(e) => local == e,
        }
    }
}

/// A role, with qualified names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Role {
    /// `<product>.<name>`.
    pub name: String,
    /// Qualified scope types the role may be bound at.
    pub at: Vec<String>,
    pub(crate) permissions: Vec<Pattern>,
    pub protection: Option<Protection>,
    pub implicit: bool,
}

/// A checked manifest, with qualified names.
#[derive(Debug, Clone, PartialEq)]
pub struct Manifest {
    product: String,
    /// Qualified scope type → qualified parent type.
    scope_parents: BTreeMap<String, String>,
    roles: BTreeMap<String, Role>,
    /// Qualified scope types custom roles may be bound at; `None` when custom roles are off.
    custom_at: Option<Vec<String>>,
    /// Qualified permission needed to give or take a role, by qualified role.
    grant: BTreeMap<String, String>,
    grant_default: String,
    source: ManifestSource,
}

pub(crate) fn is_ident(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some('a'..='z')) && chars.all(|c| matches!(c, 'a'..='z' | '0'..='9' | '_'))
}

/// Identifiers joined by dots.
pub(crate) fn is_permission_name(s: &str) -> bool {
    !s.is_empty() && s.split('.').all(is_ident)
}

fn pattern(s: &str) -> Result<Pattern, ManifestError> {
    if s == "*" {
        return Ok(Pattern::All);
    }
    if let Some(prefix) = s.strip_suffix(".*")
        && is_permission_name(prefix)
    {
        return Ok(Pattern::Prefix(format!("{prefix}.")));
    }
    if is_permission_name(s) {
        return Ok(Pattern::Exact(s.to_owned()));
    }
    Err(ManifestError::BadPermission(s.to_owned()))
}

impl Manifest {
    /// Checks a manifest and qualifies its names. The order of the checks is part of the
    /// engine's specification: product, scope type names, parents, cycles, roles (sorted by
    /// name), custom roles, grant.
    pub fn from_source(source: ManifestSource) -> Result<Self, ManifestError> {
        let product = source.product.clone();
        if !is_ident(&product) || RESERVED_PRODUCTS.contains(&product.as_str()) {
            return Err(ManifestError::BadProduct(product));
        }
        let qualify = |local: &str| format!("{product}.{local}");

        for name in source.scope_types.keys() {
            if !is_ident(name) || name == PLATFORM || name == ORGANISATION {
                return Err(ManifestError::BadName(name.clone()));
            }
        }
        // A scope type named in the manifest: a built-in, a local name, or the qualified form
        // of a local name.
        let resolve_type = |name: &str| -> Option<String> {
            if name == PLATFORM || name == ORGANISATION {
                return Some(name.to_owned());
            }
            let local = name.strip_prefix(&format!("{product}.")).unwrap_or(name);
            source.scope_types.contains_key(local).then(|| qualify(local))
        };
        let mut scope_parents = BTreeMap::new();
        for (name, st) in &source.scope_types {
            let parent = resolve_type(&st.parent)
                .ok_or_else(|| ManifestError::UnknownParent { kind: name.clone(), parent: st.parent.clone() })?;
            scope_parents.insert(qualify(name), parent);
        }
        for start in scope_parents.keys() {
            let mut seen = BTreeSet::new();
            let mut at = start.as_str();
            while let Some(parent) = scope_parents.get(at) {
                if !seen.insert(at) {
                    return Err(ManifestError::ParentCycle(start.clone()));
                }
                at = parent;
            }
        }

        let mut roles = BTreeMap::new();
        for (name, r) in &source.roles {
            if !is_ident(name) || name == "custom" {
                return Err(ManifestError::BadName(name.clone()));
            }
            let mut at = Vec::new();
            for kind in &r.at {
                let resolved = resolve_type(kind)
                    .ok_or_else(|| ManifestError::UnknownScopeType { role: name.clone(), kind: kind.clone() })?;
                at.push(resolved);
            }
            let permissions = r.permissions.iter().map(|p| pattern(p)).collect::<Result<Vec<_>, _>>()?;
            let protection = match &r.protected {
                ProtectedSource::Flag(false) => None,
                ProtectedSource::Flag(true) => Some(Protection::Always),
                ProtectedSource::Bypass { bypass } => {
                    if !is_permission_name(bypass) {
                        return Err(ManifestError::BadPermission(bypass.clone()));
                    }
                    Some(Protection::Bypass(qualify(bypass)))
                }
            };
            if r.implicit && (at.len() != 1 || at[0] != PLATFORM) {
                return Err(ManifestError::ImplicitNotPlatform(name.clone()));
            }
            roles
                .insert(qualify(name), Role { name: qualify(name), at, permissions, protection, implicit: r.implicit });
        }

        let custom_at = match &source.custom_roles {
            None => None,
            Some(c) => {
                let mut at = Vec::new();
                for kind in &c.at {
                    let resolved = resolve_type(kind).ok_or_else(|| ManifestError::UnknownScopeType {
                        role: "custom".to_owned(),
                        kind: kind.clone(),
                    })?;
                    at.push(resolved);
                }
                Some(at)
            }
        };

        let mut grant = BTreeMap::new();
        let mut grant_default = None;
        for (role, permission) in &source.grant {
            if !is_permission_name(permission) {
                return Err(ManifestError::BadPermission(permission.clone()));
            }
            if role == "default" {
                grant_default = Some(qualify(permission));
            } else if source.roles.contains_key(role) {
                grant.insert(qualify(role), qualify(permission));
            } else {
                return Err(ManifestError::UnknownGrantRole(role.clone()));
            }
        }
        let grant_default = grant_default.ok_or(ManifestError::MissingGrantDefault)?;

        Ok(Manifest { product, scope_parents, roles, custom_at, grant, grant_default, source })
    }

    /// Reads and checks a manifest written in YAML.
    #[cfg(feature = "yaml")]
    pub fn from_yaml(text: &str) -> Result<Self, ManifestError> {
        let source: ManifestSource = crate::read_yaml(text).map_err(ManifestError::BadManifest)?;
        Self::from_source(source)
    }

    pub fn product(&self) -> &str {
        &self.product
    }

    /// The manifest as written, to send it on (the catalogue push) or show it.
    pub fn source(&self) -> &ManifestSource {
        &self.source
    }

    /// The roles, by qualified name.
    pub fn roles(&self) -> impl Iterator<Item = &Role> {
        self.roles.values()
    }

    pub fn role(&self, qualified: &str) -> Option<&Role> {
        self.roles.get(qualified)
    }

    /// The qualified scope types the product registers, with their parents.
    pub fn scope_types(&self) -> impl Iterator<Item = (&str, &str)> {
        self.scope_parents.iter().map(|(k, p)| (k.as_str(), p.as_str()))
    }

    pub(crate) fn scope_parent(&self, kind: &str) -> Option<&str> {
        self.scope_parents.get(kind).map(String::as_str)
    }

    pub(crate) fn custom_at(&self) -> Option<&[String]> {
        self.custom_at.as_deref()
    }

    /// The qualified permission needed to give or take `role`.
    pub fn grant_permission(&self, role: &str) -> &str {
        self.grant.get(role).unwrap_or(&self.grant_default)
    }

    /// The qualified permission needed to give or take a role without a rule of its own: what
    /// the product's administrators hold.
    pub fn grant_default(&self) -> &str {
        &self.grant_default
    }

    /// The qualified scope types custom roles may be bound at; `None` when the product has none.
    pub fn custom_roles_at(&self) -> Option<&[String]> {
        self.custom_at.as_deref()
    }
}

impl Role {
    /// Whether the role grants this local permission name of its product.
    pub(crate) fn grants_local(&self, local: &str) -> bool {
        self.permissions.iter().any(|p| p.matches(local))
    }

    pub fn is_protected(&self) -> bool {
        self.protection.is_some()
    }
}
