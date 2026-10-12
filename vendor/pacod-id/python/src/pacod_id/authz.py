"""The rules engine of pacod-id, in Python.

The same semantics as the Rust crate pacod-id-authz and the TypeScript package @pacod/id-authz;
the vectors in policy/engine-vectors/ keep the three in step. Scopes are strings: ``platform``,
or ``<type>:<id>``; a path lists a scope's ancestors root first.
"""

from __future__ import annotations

import re
from collections.abc import Iterable, Mapping, Sequence
from dataclasses import dataclass, field
from typing import Any, Literal

import yaml

PLATFORM = "platform"
ORGANISATION = "organisation"
_RESERVED_PRODUCTS = (PLATFORM, ORGANISATION, "custom")
_IDENT = re.compile(r"[a-z][a-z0-9_]*")

ManifestErrorCode = Literal[
    "bad_manifest",
    "bad_product",
    "bad_name",
    "unknown_parent",
    "parent_cycle",
    "unknown_scope_type",
    "bad_permission",
    "implicit_not_platform",
    "unknown_grant_role",
    "missing_grant_default",
]
GrantErrorCode = Literal[
    "unknown_role", "implicit_role", "bad_path", "scope_type_not_allowed", "not_permitted"
]
SubjectKind = Literal["user", "service"]


class _Yaml12Loader(yaml.SafeLoader):
    """YAML as every port of the engine reads it: only ``true`` and ``false`` are booleans (YAML
    1.2), not ``yes``, ``no``, ``on`` or ``off`` (YAML 1.1, PyYAML's default)."""


_Yaml12Loader.yaml_implicit_resolvers = {
    first: [r for r in resolvers if r[0] != "tag:yaml.org,2002:bool"]
    for first, resolvers in yaml.SafeLoader.yaml_implicit_resolvers.items()
}
_Yaml12Loader.add_implicit_resolver(
    "tag:yaml.org,2002:bool", re.compile(r"^(?:true|True|TRUE|false|False|FALSE)$"), list("tTfF")
)


def load_yaml(text: str) -> Any:
    """Reads YAML with YAML 1.2 booleans, as the Rust and TypeScript ports do."""
    # A SafeLoader subclass: no arbitrary objects.
    return yaml.load(text, Loader=_Yaml12Loader)


class ManifestError(ValueError):
    """Why a manifest was refused; ``code`` is the same in every port of the engine."""

    def __init__(self, code: ManifestErrorCode, message: str):
        super().__init__(message)
        self.code = code


def is_ident(s: str) -> bool:
    return _IDENT.fullmatch(s) is not None


def is_permission_name(s: str) -> bool:
    """Identifiers joined by dots."""
    return bool(s) and all(is_ident(part) for part in s.split("."))


@dataclass(frozen=True)
class _Pattern:
    kind: Literal["all", "prefix", "exact"]
    value: str = ""

    def matches(self, local: str) -> bool:
        if self.kind == "all":
            return True
        if self.kind == "prefix":
            return len(local) > len(self.value) and local.startswith(self.value)
        return local == self.value


def _pattern(s: str) -> _Pattern:
    if s == "*":
        return _Pattern("all")
    if s.endswith(".*") and is_permission_name(s[:-2]):
        return _Pattern("prefix", s[:-1])
    if is_permission_name(s):
        return _Pattern("exact", s)
    raise ManifestError("bad_permission", f"{s!r} is not a valid permission")


@dataclass(frozen=True)
class Role:
    """A role, with qualified names."""

    name: str
    at: tuple[str, ...]
    patterns: tuple[_Pattern, ...]
    #: None, ("always",) or ("bypass", qualified permission).
    protection: tuple[str, ...] | None
    implicit: bool

    def grants_local(self, local: str) -> bool:
        return any(p.matches(local) for p in self.patterns)


def _shape(ok: bool, what: str) -> None:
    if not ok:
        raise ManifestError("bad_manifest", f"the manifest does not have the expected shape: {what}")


def _only_keys(o: Mapping[str, Any], allowed: Iterable[str], where: str) -> None:
    for key in o:
        _shape(key in allowed, f"unknown field {key} in {where}")


def _strings(v: Any, what: str) -> list[str]:
    _shape(isinstance(v, list) and all(isinstance(x, str) for x in v), f"{what} must be a list of strings")
    return list(v)


def _read_source(raw: Any) -> dict[str, Any]:
    """Checks the shape of untrusted input, as serde does for the Rust port."""
    _shape(isinstance(raw, dict), "not a map")
    _only_keys(raw, ("product", "scope_types", "roles", "custom_roles", "grant"), "the manifest")
    _shape(isinstance(raw.get("product"), str), "product must be a string")
    src: dict[str, Any] = {"product": raw["product"]}
    if "scope_types" in raw:
        _shape(isinstance(raw["scope_types"], dict), "scope_types must be a map")
        types = {}
        for name, st in raw["scope_types"].items():
            _shape(isinstance(name, str) and isinstance(st, dict), f"scope type {name}")
            _only_keys(st, ("parent",), f"scope type {name}")
            _shape(isinstance(st.get("parent"), str), f"scope type {name} needs a parent")
            types[name] = {"parent": st["parent"]}
        src["scope_types"] = types
    if "roles" in raw:
        _shape(isinstance(raw["roles"], dict), "roles must be a map")
        roles = {}
        for name, r in raw["roles"].items():
            _shape(isinstance(name, str) and isinstance(r, dict), f"role {name}")
            _only_keys(r, ("at", "permissions", "protected", "implicit"), f"role {name}")
            out: dict[str, Any] = {"at": _strings(r.get("at"), f"role {name}: at")}
            if "permissions" in r:
                out["permissions"] = _strings(r["permissions"], f"role {name}: permissions")
            if "protected" in r:
                p = r["protected"]
                if isinstance(p, bool):
                    out["protected"] = p
                else:
                    _shape(isinstance(p, dict), f"role {name}: protected")
                    _only_keys(p, ("bypass",), f"role {name}: protected")
                    _shape(isinstance(p.get("bypass"), str), f"role {name}: protected.bypass")
                    out["protected"] = {"bypass": p["bypass"]}
            if "implicit" in r:
                _shape(isinstance(r["implicit"], bool), f"role {name}: implicit")
                out["implicit"] = r["implicit"]
            roles[name] = out
        src["roles"] = roles
    if "custom_roles" in raw:
        c = raw["custom_roles"]
        _shape(isinstance(c, dict), "custom_roles must be a map")
        _only_keys(c, ("at",), "custom_roles")
        src["custom_roles"] = {"at": _strings(c.get("at"), "custom_roles: at")}
    if "grant" in raw:
        g = raw["grant"]
        _shape(isinstance(g, dict), "grant must be a map")
        for k, v in g.items():
            _shape(isinstance(k, str) and isinstance(v, str), f"grant.{k} must be a string")
        src["grant"] = dict(g)
    return src


class Manifest:
    """A checked manifest, with qualified names."""

    def __init__(
        self,
        product: str,
        scope_parents: dict[str, str],
        roles: dict[str, Role],
        custom_at: tuple[str, ...] | None,
        grant: dict[str, str],
        grant_default: str,
        source: dict[str, Any],
    ):
        self.product = product
        self._scope_parents = scope_parents
        self._roles = roles
        #: Qualified scope types custom roles may be bound at; None when custom roles are off.
        self.custom_at = custom_at
        self._grant = grant
        self._grant_default = grant_default
        #: The manifest as written, to send it on (the catalogue push) or show it.
        self.source = source

    @classmethod
    def from_source(cls, raw: Any) -> Manifest:
        """Checks a manifest and qualifies its names, in the order the engine's specification
        gives: product, scope type names, parents, cycles, roles (sorted), custom roles, grant."""
        src = _read_source(raw)
        product: str = src["product"]
        if not is_ident(product) or product in _RESERVED_PRODUCTS:
            raise ManifestError("bad_product", f"{product!r} is not a valid product name")

        def qualify(local: str) -> str:
            return f"{product}.{local}"

        scope_types: dict[str, dict[str, str]] = src.get("scope_types", {})
        for name in sorted(scope_types):
            if not is_ident(name) or name in (PLATFORM, ORGANISATION):
                raise ManifestError("bad_name", f"{name!r} is not a valid name")

        def resolve_type(name: str) -> str | None:
            if name in (PLATFORM, ORGANISATION):
                return name
            local = name[len(product) + 1 :] if name.startswith(f"{product}.") else name
            return qualify(local) if local in scope_types else None

        scope_parents: dict[str, str] = {}
        for name in sorted(scope_types):
            parent = resolve_type(scope_types[name]["parent"])
            if parent is None:
                raise ManifestError("unknown_parent", f"scope type {name} has an unknown parent")
            scope_parents[qualify(name)] = parent
        for start in sorted(scope_parents):
            seen: set[str] = set()
            at: str | None = start
            while at is not None and at in scope_parents:
                if at in seen:
                    raise ManifestError("parent_cycle", f"scope type {start} is its own ancestor")
                seen.add(at)
                at = scope_parents.get(at)

        roles: dict[str, Role] = {}
        role_sources: dict[str, dict[str, Any]] = src.get("roles", {})
        for name in sorted(role_sources):
            r = role_sources[name]
            if not is_ident(name) or name == "custom":
                raise ManifestError("bad_name", f"{name!r} is not a valid name")
            at_list = []
            for kind in r["at"]:
                resolved = resolve_type(kind)
                if resolved is None:
                    raise ManifestError(
                        "unknown_scope_type", f"role {name} names an unknown scope type {kind}"
                    )
                at_list.append(resolved)
            patterns = tuple(_pattern(p) for p in r.get("permissions", []))
            protection: tuple[str, ...] | None = None
            prot = r.get("protected", False)
            if prot is True:
                protection = ("always",)
            elif isinstance(prot, dict):
                if not is_permission_name(prot["bypass"]):
                    raise ManifestError("bad_permission", f"{prot['bypass']!r} is not a valid permission")
                protection = ("bypass", qualify(prot["bypass"]))
            implicit = r.get("implicit", False) is True
            if implicit and at_list != [PLATFORM]:
                raise ManifestError(
                    "implicit_not_platform", f"implicit role {name} must be bound at the platform"
                )
            roles[qualify(name)] = Role(qualify(name), tuple(at_list), patterns, protection, implicit)

        custom_at: tuple[str, ...] | None = None
        if "custom_roles" in src:
            resolved_types = []
            for kind in src["custom_roles"]["at"]:
                resolved = resolve_type(kind)
                if resolved is None:
                    raise ManifestError(
                        "unknown_scope_type", f"custom roles name an unknown scope type {kind}"
                    )
                resolved_types.append(resolved)
            custom_at = tuple(resolved_types)

        grant: dict[str, str] = {}
        grant_default: str | None = None
        grant_source: dict[str, str] = src.get("grant", {})
        for role in sorted(grant_source):
            permission = grant_source[role]
            if not is_permission_name(permission):
                raise ManifestError("bad_permission", f"{permission!r} is not a valid permission")
            if role == "default":
                grant_default = qualify(permission)
            elif role in role_sources:
                grant[qualify(role)] = qualify(permission)
            else:
                raise ManifestError("unknown_grant_role", f"grant names an unknown role {role}")
        if grant_default is None:
            raise ManifestError("missing_grant_default", "grant has no default")

        return cls(product, scope_parents, roles, custom_at, grant, grant_default, src)

    @classmethod
    def from_yaml(cls, text: str) -> Manifest:
        """Reads and checks a manifest written in YAML."""
        try:
            raw = load_yaml(text)
        except yaml.YAMLError as e:
            raise ManifestError("bad_manifest", f"not YAML: {e}") from e
        return cls.from_source(raw)

    def roles(self) -> list[Role]:
        """The roles, sorted by qualified name."""
        return [self._roles[k] for k in sorted(self._roles)]

    def role(self, qualified: str) -> Role | None:
        return self._roles.get(qualified)

    def scope_types(self) -> list[tuple[str, str]]:
        """The qualified scope types the product registers, with their parents."""
        return sorted(self._scope_parents.items())

    def scope_parent(self, kind: str) -> str | None:
        return self._scope_parents.get(kind)

    def grant_permission(self, role: str) -> str:
        """The qualified permission needed to give or take ``role``."""
        return self._grant.get(role, self._grant_default)


@dataclass(frozen=True)
class Binding:
    """A role held at a scope, with the scope's path (the scope is the last element)."""

    role: str
    path: tuple[str, ...]

    @classmethod
    def of(cls, data: Mapping[str, Any]) -> Binding:
        return cls(data["role"], tuple(data["path"]))


@dataclass(frozen=True)
class HeldBinding:
    """A binding with its holder, for the last-admin check."""

    principal: str
    role: str
    path: tuple[str, ...]

    @classmethod
    def of(cls, data: Mapping[str, Any]) -> HeldBinding:
        return cls(data["principal"], data["role"], tuple(data["path"]))


@dataclass(frozen=True)
class Subject:
    """Who is checked: a person's account (which holds the implicit roles) or a service account."""

    kind: SubjectKind
    bindings: tuple[Binding, ...] = field(default_factory=tuple)

    @classmethod
    def of(cls, data: Mapping[str, Any]) -> Subject:
        return cls(data["kind"], tuple(Binding.of(b) for b in data.get("bindings", [])))


@dataclass(frozen=True)
class Violation:
    """A change that would leave a scope without a holder of a protected role."""

    role: str
    scope: str


def scope_kind(scope: str) -> str | None:
    """The type of a scope written as text, or None when it is not ``platform`` or ``type:id``."""
    if scope == PLATFORM:
        return PLATFORM
    kind, sep, ident = scope.partition(":")
    if not sep or not kind or not ident or kind == PLATFORM:
        return None
    return kind


def _is_prefix(prefix: Sequence[str], path: Sequence[str]) -> bool:
    return 0 < len(prefix) <= len(path) and all(a == b for a, b in zip(prefix, path, strict=False))


def _split(name: str) -> tuple[str, str] | None:
    product, sep, rest = name.partition(".")
    return (product, rest) if sep else None


class Catalog:
    """Every product's manifest, by product name."""

    def __init__(self, manifests: Iterable[Manifest] = ()):
        self._manifests: dict[str, Manifest] = {}
        for m in manifests:
            self.insert(m)

    def insert(self, manifest: Manifest) -> None:
        """Adds a manifest, replacing the product's earlier one."""
        self._manifests[manifest.product] = manifest

    def manifest(self, product: str) -> Manifest | None:
        return self._manifests.get(product)

    def _resolve(self, role: str) -> Role | tuple[str, ...] | None:
        """A defined Role, the scope types of a custom role, or None."""
        parts = _split(role)
        if parts is None:
            return None
        manifest = self._manifests.get(parts[0])
        if manifest is None:
            return None
        if parts[1].startswith("custom."):
            name = parts[1][len("custom.") :]
            return manifest.custom_at if is_ident(name) and manifest.custom_at is not None else None
        return manifest.role(role)

    def _parent_type(self, kind: str) -> str | None:
        if kind == ORGANISATION:
            return PLATFORM
        parts = _split(kind)
        if parts is None:
            return None
        manifest = self._manifests.get(parts[0])
        return manifest.scope_parent(kind) if manifest else None

    def is_valid_path(self, path: Sequence[str]) -> bool:
        """Whether a path leads from the platform through the scope types, one level at a time."""
        if not path or path[0] != PLATFORM:
            return False
        previous = PLATFORM
        for scope in path[1:]:
            kind = scope_kind(scope)
            if kind is None or kind == PLATFORM or self._parent_type(kind) != previous:
                return False
            previous = kind
        return True

    def _effective(self, role: str, path: Sequence[str]) -> Role | tuple[str, ...] | None:
        """The role of a binding, when the binding counts; others are ignored by every check."""
        if not path or path[0] != PLATFORM:
            return None
        kind = scope_kind(path[-1])
        if kind is None:
            return None
        resolved = self._resolve(role)
        if resolved is None:
            return None
        at = resolved.at if isinstance(resolved, Role) else resolved
        return resolved if kind in at else None

    def _implicit_roles(self, subject: Subject, product: str) -> list[Role]:
        if subject.kind != "user":
            return []
        manifest = self._manifests.get(product)
        return [r for r in manifest.roles() if r.implicit] if manifest else []

    @staticmethod
    def _grants(product: str, local: str, role: Role) -> bool:
        return role.name.partition(".")[0] == product and role.grants_local(local)

    def allowed(self, subject: Subject, permission: str, at: Sequence[str]) -> bool:
        """Whether the subject may do ``permission`` (qualified) at the scope with path ``at``."""
        parts = _split(permission)
        if parts is None:
            return False
        product, local = parts
        for b in subject.bindings:
            r = self._effective(b.role, b.path)
            if isinstance(r, Role) and _is_prefix(b.path, at) and self._grants(product, local, r):
                return True
        return (
            bool(at)
            and at[0] == PLATFORM
            and any(self._grants(product, local, r) for r in self._implicit_roles(subject, product))
        )

    def allowed_somewhere(self, subject: Subject, permission: str) -> bool:
        """Whether some binding of the subject grants ``permission``, wherever it is held."""
        parts = _split(permission)
        if parts is None:
            return False
        product, local = parts
        for b in subject.bindings:
            r = self._effective(b.role, b.path)
            if isinstance(r, Role) and self._grants(product, local, r):
                return True
        return any(self._grants(product, local, r) for r in self._implicit_roles(subject, product))

    def has_role(self, subject: Subject, role: str, scope: str) -> bool:
        """Whether the subject holds exactly ``role`` at exactly ``scope``."""
        for b in subject.bindings:
            if (
                b.role == role
                and b.path
                and b.path[-1] == scope
                and self._effective(b.role, b.path) is not None
            ):
                return True
        parts = _split(role)
        return (
            scope == PLATFORM
            and parts is not None
            and any(r.name == role for r in self._implicit_roles(subject, parts[0]))
        )

    def holds_any_at(self, subject: Subject, scope: str) -> bool:
        """Whether the subject holds some role at exactly ``scope``."""
        return any(
            b.path and b.path[-1] == scope and self._effective(b.role, b.path) is not None
            for b in subject.bindings
        )

    def touches(self, subject: Subject, at: Sequence[str]) -> bool:
        """Whether some binding of the subject sits on ``at``, above it or below it."""
        return any(
            self._effective(b.role, b.path) is not None and (_is_prefix(b.path, at) or _is_prefix(at, b.path))
            for b in subject.bindings
        )

    def can_grant(self, actor: Subject, role: str, at: Sequence[str]) -> GrantErrorCode | None:
        """Whether ``actor`` may give (or take) ``role`` at the scope with path ``at``; None when it may."""
        resolved = self._resolve(role)
        if resolved is None:
            return "unknown_role"
        if isinstance(resolved, Role) and resolved.implicit:
            return "implicit_role"
        if not self.is_valid_path(at):
            return "bad_path"
        allowed_at = resolved.at if isinstance(resolved, Role) else resolved
        if scope_kind(at[-1]) not in allowed_at:
            return "scope_type_not_allowed"
        manifest = self._manifests[role.partition(".")[0]]
        return None if self.allowed(actor, manifest.grant_permission(role), at) else "not_permitted"

    def check_protected(
        self, actor: Subject, before: Sequence[HeldBinding], after: Sequence[HeldBinding]
    ) -> Violation | None:
        """Refuses a change of bindings, from ``before`` to ``after``, that leaves a scope without a
        holder of a protected role, unless the role's bypass permission lets ``actor`` do it. Returns
        the first violation, ordered by role, then scope; None when the change may go."""
        checked: dict[tuple[str, str], tuple[Role, tuple[str, ...]]] = {}
        for b in before:
            r = self._effective(b.role, b.path)
            if isinstance(r, Role) and r.protection is not None:
                checked.setdefault((b.role, b.path[-1]), (r, b.path))
        for (name, scope), (role, path) in sorted(checked.items()):
            left = any(
                b.role == name
                and b.path
                and b.path[-1] == scope
                and self._effective(b.role, b.path) is not None
                for b in after
            )
            if left:
                continue
            bypassed = (
                role.protection is not None
                and role.protection[0] == "bypass"
                and self.allowed(actor, role.protection[1], path)
            )
            if not bypassed:
                return Violation(name, scope)
        return None
