# Rules engine test vectors

Every port of the rules engine (Rust `pacod-id-authz`, TypeScript `@pacod/id-authz`, Python
`pacod_id.authz`) loads these files and must give the same answers. They are the engine's
specification; the code follows them, not the other way round.

## Terms

- **Manifest**: a product's roles, permissions and scope types (`manifests/*.yaml`).
- **Scope**: `platform`, `organisation:<id>`, or a product type with an id, e.g. `acme.workspace:w1`.
- **Path**: a scope with all its ancestors, root first: `[platform, "organisation:o1", "acme.workspace:w1"]`.
- **Binding**: a role held at a scope, written with its path: `{ role: acme.org_admin, path: [platform, "organisation:o1"] }`.
- **Subject**: who is checked: `{ kind: user | service, bindings: [...] }`.

Roles and permissions are written fully qualified in vectors (`acme.org_admin`,
`acme.org.manage`). Inside a manifest they are local (`org_admin`, `org.manage`).

## Semantics

- `allowed(permission, at)`: some binding of the subject sits on `at` or one of its ancestors
  (its path is a prefix of `at`), and its role grants the permission.
- **Implicit roles**: a role marked `implicit` counts as bound at `platform` for every subject of
  kind `user`, but only in checks that name that role or a permission of its product:
  `allowed`, `allowed_somewhere` and `has_role`. `touches` and `holds_any_at` look at stored
  bindings only, since an implicit role says nothing about where someone works.
- A role grants a permission of its own product only. Patterns: `*` (every permission of the
  product), `x.*` (every permission starting with `x.`), or an exact name.
- A binding whose role is unknown, or whose scope type is not among the role's `at`, grants
  nothing.
- `has_role(role, scope)`: the subject holds exactly this role at exactly this scope.
- `holds_any_at(scope)`: the subject holds some role at exactly this scope.
- `allowed_somewhere(permission)`: some binding of the subject grants the permission, wherever.
- `touches(at)`: some binding of the subject sits on `at`, above it or below it.
- `can_grant(role, at)`, for an actor: answers `ok` or an error code:
  - `unknown_role`: no such role; custom roles exist only where the manifest allows them;
  - `implicit_role`: implicit roles are never bound;
  - `bad_path`: the path does not start at `platform`, or an element's type is not the child of
    the previous element's type;
  - `scope_type_not_allowed`: the role may not be bound at this scope type;
  - `not_permitted`: the actor lacks the grant permission (`grant.<role>`, else
    `grant.default`) at `at`.
- `check_protected(before, after)`, for an actor: for each role marked `protected` and each
  scope where `before` holds at least one binding of it and `after` holds none, the change is
  refused, unless the role names a `bypass` permission that the actor is allowed at that scope.
  The answer is `ok` or the first violation `{ role, scope }`, ordered by role, then scope.
- Custom roles are written `<product>.custom.<name>`. They exist where the manifest has
  `custom_roles`, may be bound at its `at` types, give no permission, and count for `has_role`.

## Manifest errors

Loading a manifest fails with one of these codes:

| Code                    | When                                                                                                                                     |
| ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
| `bad_product`           | the product name is not a lowercase identifier, or is `platform`, `organisation` or `custom`                                             |
| `bad_name`              | a role or scope type name is not a lowercase identifier, a role is named `custom`, or a scope type is named `platform` or `organisation` |
| `unknown_parent`        | a scope type's parent is neither `platform`, `organisation` nor a type of the manifest                                                   |
| `parent_cycle`          | scope types are their own ancestors                                                                                                      |
| `unknown_scope_type`    | a role's `at` (or `custom_roles.at`) names an unknown scope type                                                                         |
| `bad_permission`        | a permission, grant permission or bypass permission is not a valid name or pattern                                                       |
| `implicit_not_platform` | an implicit role is bound anywhere but `platform` only                                                                                   |
| `unknown_grant_role`    | `grant` names a role the manifest does not have                                                                                          |
| `missing_grant_default` | `grant.default` is missing                                                                                                               |

Manifests and vectors are YAML 1.2: only `true` and `false` are booleans, so `implicit: yes` is
refused (`bad_manifest`); every port reads YAML that way.

A lowercase identifier is `[a-z][a-z0-9_]*`. A permission name is identifiers joined by dots.
A pattern is `*`, or a permission name followed by `.*`.

## Files

| File                   | Holds                                                                 |
| ---------------------- | --------------------------------------------------------------------- |
| `evaluation.yaml`      | `allowed`, `has_role`, `holds_any_at`, `allowed_somewhere`, `touches` |
| `grants.yaml`          | `can_grant`                                                           |
| `protected.yaml`       | `check_protected`                                                     |
| `manifest-errors.yaml` | manifests that must be refused, with the code                         |

Each of the first three files lists the manifests it loads (`manifests: [acme, shop]`) and
`cases`. A case has a `subject` (or `actor`) and `checks`; every check has an `expect`.
