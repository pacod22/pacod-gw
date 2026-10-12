//! Evaluating bindings against the catalogue of manifests (`policy/engine-vectors/README.md`).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::manifest::{Manifest, Protection, Role, is_ident};
use crate::scope::{ORGANISATION, PLATFORM, ScopeRef, is_prefix};

/// A role held at a scope, with the scope's path (root first; the scope is the last element).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Binding {
    pub role: String,
    pub path: Vec<ScopeRef>,
}

impl Binding {
    pub fn new(role: impl Into<String>, path: Vec<ScopeRef>) -> Self {
        Binding { role: role.into(), path }
    }

    /// The scope the role is held at.
    pub fn scope(&self) -> Option<&ScopeRef> {
        self.path.last()
    }
}

/// A binding with its holder, for the last-admin check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeldBinding {
    pub principal: String,
    pub role: String,
    pub path: Vec<ScopeRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubjectKind {
    /// A person's account. Holds the implicit roles.
    User,
    /// A service account.
    Service,
}

/// Who is checked: their kind and their bindings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subject {
    pub kind: SubjectKind,
    pub bindings: Vec<Binding>,
}

impl Subject {
    pub fn user(bindings: Vec<Binding>) -> Self {
        Subject { kind: SubjectKind::User, bindings }
    }

    pub fn service(bindings: Vec<Binding>) -> Self {
        Subject { kind: SubjectKind::Service, bindings }
    }
}

/// Why a role may not be given (or taken) at a scope. `code()` is the same in every port.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum GrantError {
    #[error("there is no such role")]
    UnknownRole,
    #[error("implicit roles are never bound")]
    ImplicitRole,
    #[error("the path does not lead from the platform through the scope types")]
    BadPath,
    #[error("the role cannot be given at this kind of scope")]
    ScopeTypeNotAllowed,
    #[error("not permitted to give or take this role here")]
    NotPermitted,
}

impl GrantError {
    pub fn code(&self) -> &'static str {
        match self {
            GrantError::UnknownRole => "unknown_role",
            GrantError::ImplicitRole => "implicit_role",
            GrantError::BadPath => "bad_path",
            GrantError::ScopeTypeNotAllowed => "scope_type_not_allowed",
            GrantError::NotPermitted => "not_permitted",
        }
    }
}

/// A change that would leave a scope without a holder of a protected role.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("the change would leave {scope} without a holder of {role}")]
pub struct Violation {
    pub role: String,
    pub scope: ScopeRef,
}

/// What a role name resolves to.
enum RoleRef<'a> {
    Defined(&'a Role),
    /// `<product>.custom.<name>`: held, bindable at these scope types, grants nothing.
    Custom {
        at: &'a [String],
    },
}

/// Every product's manifest, by product name.
#[derive(Debug, Clone, Default)]
pub struct Catalog {
    manifests: BTreeMap<String, Manifest>,
}

impl Catalog {
    pub fn new() -> Self {
        Catalog::default()
    }

    /// Adds a manifest, replacing the product's earlier one.
    pub fn insert(&mut self, manifest: Manifest) {
        self.manifests.insert(manifest.product().to_owned(), manifest);
    }

    pub fn with(mut self, manifest: Manifest) -> Self {
        self.insert(manifest);
        self
    }

    pub fn manifest(&self, product: &str) -> Option<&Manifest> {
        self.manifests.get(product)
    }

    pub fn manifests(&self) -> impl Iterator<Item = &Manifest> {
        self.manifests.values()
    }

    fn resolve(&self, role: &str) -> Option<RoleRef<'_>> {
        let (product, rest) = role.split_once('.')?;
        let manifest = self.manifests.get(product)?;
        if let Some(name) = rest.strip_prefix("custom.") {
            return (is_ident(name)).then_some(()).and(manifest.custom_at()).map(|at| RoleRef::Custom { at });
        }
        manifest.role(role).map(RoleRef::Defined)
    }

    /// The parent type of a scope type: `None` for the platform and for unknown types.
    fn parent_type(&self, kind: &str) -> Option<&str> {
        if kind == ORGANISATION {
            return Some(PLATFORM);
        }
        let (product, _) = kind.split_once('.')?;
        self.manifests.get(product)?.scope_parent(kind)
    }

    /// Whether a path leads from the platform through the scope types, one level at a time.
    pub fn is_valid_path(&self, path: &[ScopeRef]) -> bool {
        let Some((first, rest)) = path.split_first() else { return false };
        if !first.is_platform() {
            return false;
        }
        let mut previous = PLATFORM;
        for scope in rest {
            if scope.id.is_none() || self.parent_type(&scope.kind) != Some(previous) {
                return false;
            }
            previous = &scope.kind;
        }
        true
    }

    /// The role of a binding, when the binding counts: the role is known and may be held at
    /// the binding's scope type. Other bindings are ignored by every check.
    fn effective<'a>(&'a self, b: &Binding) -> Option<RoleRef<'a>> {
        let scope = b.scope()?;
        if !b.path.first().is_some_and(ScopeRef::is_platform) {
            return None;
        }
        let role = self.resolve(&b.role)?;
        let at: &[String] = match &role {
            RoleRef::Defined(r) => &r.at,
            RoleRef::Custom { at } => at,
        };
        at.contains(&scope.kind).then_some(role)
    }

    /// The implicit roles of a permission's or a role's product that `subject` holds.
    fn implicit_roles<'a>(&'a self, subject: &Subject, product: &str) -> impl Iterator<Item = &'a Role> {
        let manifest = (subject.kind == SubjectKind::User).then(|| self.manifests.get(product)).flatten();
        manifest.into_iter().flat_map(|m| m.roles().filter(|r| r.implicit))
    }

    /// Whether the subject may do `permission` (qualified) at the scope with path `at`.
    pub fn allowed(&self, subject: &Subject, permission: &str, at: &[ScopeRef]) -> bool {
        let Some((product, local)) = permission.split_once('.') else { return false };
        let grants =
            |role: &Role| role.name.split_once('.').is_some_and(|(p, _)| p == product) && role.grants_local(local);
        let stored = subject.bindings.iter().any(|b| match self.effective(b) {
            Some(RoleRef::Defined(role)) => is_prefix(&b.path, at) && grants(role),
            _ => false,
        });
        stored || (at.first().is_some_and(ScopeRef::is_platform) && self.implicit_roles(subject, product).any(grants))
    }

    /// Whether some binding of the subject grants `permission`, wherever it is held.
    pub fn allowed_somewhere(&self, subject: &Subject, permission: &str) -> bool {
        let Some((product, local)) = permission.split_once('.') else { return false };
        let grants =
            |role: &Role| role.name.split_once('.').is_some_and(|(p, _)| p == product) && role.grants_local(local);
        subject.bindings.iter().any(|b| matches!(self.effective(b), Some(RoleRef::Defined(role)) if grants(role)))
            || self.implicit_roles(subject, product).any(grants)
    }

    /// Whether the subject holds exactly `role` at exactly `scope`.
    pub fn has_role(&self, subject: &Subject, role: &str, scope: &ScopeRef) -> bool {
        let stored =
            subject.bindings.iter().any(|b| b.role == role && b.scope() == Some(scope) && self.effective(b).is_some());
        let implicit = scope.is_platform()
            && role
                .split_once('.')
                .is_some_and(|(product, _)| self.implicit_roles(subject, product).any(|r| r.name == role));
        stored || implicit
    }

    /// Whether the subject holds some role at exactly `scope`.
    pub fn holds_any_at(&self, subject: &Subject, scope: &ScopeRef) -> bool {
        subject.bindings.iter().any(|b| b.scope() == Some(scope) && self.effective(b).is_some())
    }

    /// Whether some binding of the subject sits on `at`, above it or below it.
    pub fn touches(&self, subject: &Subject, at: &[ScopeRef]) -> bool {
        subject
            .bindings
            .iter()
            .any(|b| self.effective(b).is_some() && (is_prefix(&b.path, at) || is_prefix(at, &b.path)))
    }

    /// Whether `actor` may give (or take) `role` at the scope with path `at`.
    pub fn can_grant(&self, actor: &Subject, role: &str, at: &[ScopeRef]) -> Result<(), GrantError> {
        let resolved = self.resolve(role).ok_or(GrantError::UnknownRole)?;
        if matches!(resolved, RoleRef::Defined(r) if r.implicit) {
            return Err(GrantError::ImplicitRole);
        }
        if !self.is_valid_path(at) {
            return Err(GrantError::BadPath);
        }
        let kind = &at.last().ok_or(GrantError::BadPath)?.kind;
        let allowed_at: &[String] = match &resolved {
            RoleRef::Defined(r) => &r.at,
            RoleRef::Custom { at } => at,
        };
        if !allowed_at.iter().any(|k| k == kind) {
            return Err(GrantError::ScopeTypeNotAllowed);
        }
        let product = role.split_once('.').map(|(p, _)| p).ok_or(GrantError::UnknownRole)?;
        let manifest = self.manifests.get(product).ok_or(GrantError::UnknownRole)?;
        if self.allowed(actor, manifest.grant_permission(role), at) { Ok(()) } else { Err(GrantError::NotPermitted) }
    }

    /// Refuses a change of bindings, from `before` to `after`, that leaves a scope without a
    /// holder of a protected role, unless the role's bypass permission lets `actor` do it.
    pub fn check_protected(
        &self,
        actor: &Subject,
        before: &[HeldBinding],
        after: &[HeldBinding],
    ) -> Result<(), Violation> {
        let counts = |b: &HeldBinding| self.effective(&Binding { role: b.role.clone(), path: b.path.clone() });
        let mut checked: BTreeMap<(String, String), (&Role, &[ScopeRef])> = BTreeMap::new();
        for b in before {
            if let Some(RoleRef::Defined(role)) = counts(b)
                && role.protection.is_some()
                && let Some(scope) = b.path.last()
            {
                checked.entry((b.role.clone(), scope.to_string())).or_insert((role, &b.path));
            }
        }
        for ((role_name, _), (role, path)) in &checked {
            let scope = path.last().expect("checked bindings have a scope");
            let left =
                after.iter().any(|b| b.role == *role_name && b.path.last() == Some(scope) && counts(b).is_some());
            if left {
                continue;
            }
            let bypassed = match &role.protection {
                Some(Protection::Bypass(permission)) => self.allowed(actor, permission, path),
                _ => false,
            };
            if !bypassed {
                return Err(Violation { role: role_name.clone(), scope: scope.clone() });
            }
        }
        Ok(())
    }
}
