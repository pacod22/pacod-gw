// The rules engine of pacod-id, in TypeScript. The same semantics as the Rust crate
// pacod-id-authz and the Python module pacod_id.authz; policy/engine-vectors/ keeps them in step.
// Manifests written in YAML are read through '@pacod/id-authz/yaml', so that browser bundles
// need no YAML parser.

export {
	Manifest,
	ManifestError,
	ORGANISATION,
	PLATFORM,
	isIdent,
	isPermissionName,
	type ManifestErrorCode,
	type ManifestSource,
	type Protection,
	type Role,
	type RoleSource
} from './manifest.js';
export {
	Catalog,
	scopeKind,
	type Binding,
	type GrantErrorCode,
	type HeldBinding,
	type Subject,
	type SubjectKind,
	type Violation
} from './catalog.js';
