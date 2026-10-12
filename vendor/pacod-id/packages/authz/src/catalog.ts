// Evaluating bindings against the catalogue of manifests (policy/engine-vectors/README.md).
// Scopes are strings: `platform`, or `<type>:<id>`; a path lists a scope's ancestors root first.

import {
	compare,
	grantsLocal,
	isIdent,
	Manifest,
	ORGANISATION,
	PLATFORM,
	type Role
} from './manifest.js';

/** A role held at a scope, with the scope's path (the scope is the last element). */
export interface Binding {
	role: string;
	path: string[];
}

/** A binding with its holder, for the last-admin check. */
export interface HeldBinding extends Binding {
	principal: string;
}

export type SubjectKind = 'user' | 'service';

/** Who is checked: a person's account (which holds the implicit roles) or a service account. */
export interface Subject {
	kind: SubjectKind;
	bindings: Binding[];
}

export type GrantErrorCode =
	'unknown_role' | 'implicit_role' | 'bad_path' | 'scope_type_not_allowed' | 'not_permitted';

/** A change that would leave a scope without a holder of a protected role. */
export interface Violation {
	role: string;
	scope: string;
}

/** The type of a scope written as text, or null when it is not `platform` or `type:id`. */
export function scopeKind(scope: string): string | null {
	if (scope === PLATFORM) return PLATFORM;
	const i = scope.indexOf(':');
	if (i <= 0 || i === scope.length - 1) return null;
	const kind = scope.slice(0, i);
	return kind === PLATFORM ? null : kind;
}

function isPrefix(prefix: readonly string[], path: readonly string[]): boolean {
	return prefix.length > 0 && prefix.length <= path.length && prefix.every((s, i) => s === path[i]);
}

function split(name: string): [string, string] | null {
	const i = name.indexOf('.');
	return i < 0 ? null : [name.slice(0, i), name.slice(i + 1)];
}

type RoleRef = { kind: 'defined'; role: Role } | { kind: 'custom'; at: readonly string[] };

/** Every product's manifest, by product name. */
export class Catalog {
	private readonly manifests = new Map<string, Manifest>();

	/** Adds a manifest, replacing the product's earlier one. */
	insert(manifest: Manifest): void {
		this.manifests.set(manifest.product, manifest);
	}

	with(manifest: Manifest): this {
		this.insert(manifest);
		return this;
	}

	manifest(product: string): Manifest | undefined {
		return this.manifests.get(product);
	}

	private resolve(role: string): RoleRef | null {
		const parts = split(role);
		if (parts === null) return null;
		const manifest = this.manifests.get(parts[0]);
		if (manifest === undefined) return null;
		if (parts[1].startsWith('custom.')) {
			const name = parts[1].slice('custom.'.length);
			return isIdent(name) && manifest.customAt !== null
				? { kind: 'custom', at: manifest.customAt }
				: null;
		}
		const defined = manifest.role(role);
		return defined === undefined ? null : { kind: 'defined', role: defined };
	}

	private parentType(kind: string): string | undefined {
		if (kind === ORGANISATION) return PLATFORM;
		const parts = split(kind);
		if (parts === null) return undefined;
		return this.manifests.get(parts[0])?.scopeParent(kind);
	}

	/** Whether a path leads from the platform through the scope types, one level at a time. */
	isValidPath(path: readonly string[]): boolean {
		if (path.length === 0 || path[0] !== PLATFORM) return false;
		let previous = PLATFORM;
		for (const scope of path.slice(1)) {
			const kind = scopeKind(scope);
			if (kind === null || kind === PLATFORM || this.parentType(kind) !== previous) return false;
			previous = kind;
		}
		return true;
	}

	/** The role of a binding, when the binding counts; other bindings are ignored by every check. */
	private effective(b: Binding): RoleRef | null {
		if (b.path.length === 0 || b.path[0] !== PLATFORM) return null;
		const kind = scopeKind(b.path[b.path.length - 1]);
		if (kind === null) return null;
		const role = this.resolve(b.role);
		if (role === null) return null;
		const at = role.kind === 'defined' ? role.role.at : role.at;
		return at.includes(kind) ? role : null;
	}

	private implicitRoles(subject: Subject, product: string): Role[] {
		if (subject.kind !== 'user') return [];
		return (
			this.manifests
				.get(product)
				?.roles()
				.filter((r) => r.implicit) ?? []
		);
	}

	private grants(product: string, local: string): (role: Role) => boolean {
		return (role) => split(role.name)?.[0] === product && grantsLocal(role, local);
	}

	/** Whether the subject may do `permission` (qualified) at the scope with path `at`. */
	allowed(subject: Subject, permission: string, at: readonly string[]): boolean {
		const parts = split(permission);
		if (parts === null) return false;
		const grants = this.grants(parts[0], parts[1]);
		const stored = subject.bindings.some((b) => {
			const r = this.effective(b);
			return r?.kind === 'defined' && isPrefix(b.path, at) && grants(r.role);
		});
		return stored || (at[0] === PLATFORM && this.implicitRoles(subject, parts[0]).some(grants));
	}

	/** Whether some binding of the subject grants `permission`, wherever it is held. */
	allowedSomewhere(subject: Subject, permission: string): boolean {
		const parts = split(permission);
		if (parts === null) return false;
		const grants = this.grants(parts[0], parts[1]);
		return (
			subject.bindings.some((b) => {
				const r = this.effective(b);
				return r?.kind === 'defined' && grants(r.role);
			}) || this.implicitRoles(subject, parts[0]).some(grants)
		);
	}

	/** Whether the subject holds exactly `role` at exactly `scope`. */
	hasRole(subject: Subject, role: string, scope: string): boolean {
		const stored = subject.bindings.some(
			(b) => b.role === role && b.path[b.path.length - 1] === scope && this.effective(b) !== null
		);
		if (stored) return true;
		const product = split(role)?.[0];
		return (
			scope === PLATFORM &&
			product !== undefined &&
			this.implicitRoles(subject, product).some((r) => r.name === role)
		);
	}

	/** Whether the subject holds some role at exactly `scope`. */
	holdsAnyAt(subject: Subject, scope: string): boolean {
		return subject.bindings.some(
			(b) => b.path[b.path.length - 1] === scope && this.effective(b) !== null
		);
	}

	/** Whether some binding of the subject sits on `at`, above it or below it. */
	touches(subject: Subject, at: readonly string[]): boolean {
		return subject.bindings.some(
			(b) => this.effective(b) !== null && (isPrefix(b.path, at) || isPrefix(at, b.path))
		);
	}

	/** Whether `actor` may give (or take) `role` at the scope with path `at`; null when it may. */
	canGrant(actor: Subject, role: string, at: readonly string[]): GrantErrorCode | null {
		const resolved = this.resolve(role);
		if (resolved === null) return 'unknown_role';
		if (resolved.kind === 'defined' && resolved.role.implicit) return 'implicit_role';
		if (!this.isValidPath(at)) return 'bad_path';
		const kind = scopeKind(at[at.length - 1]);
		const allowedAt = resolved.kind === 'defined' ? resolved.role.at : resolved.at;
		if (kind === null || !allowedAt.includes(kind)) return 'scope_type_not_allowed';
		const manifest = this.manifests.get(split(role)![0])!;
		return this.allowed(actor, manifest.grantPermission(role), at) ? null : 'not_permitted';
	}

	/**
	 * Refuses a change of bindings, from `before` to `after`, that leaves a scope without a
	 * holder of a protected role, unless the role's bypass permission lets `actor` do it.
	 * Returns the first violation, ordered by role, then scope; null when the change may go.
	 */
	checkProtected(actor: Subject, before: HeldBinding[], after: HeldBinding[]): Violation | null {
		const checked = new Map<string, { role: Role; path: string[]; name: string; scope: string }>();
		for (const b of before) {
			const r = this.effective(b);
			if (r?.kind !== 'defined' || r.role.protection === null) continue;
			const scope = b.path[b.path.length - 1];
			const key = `${b.role}\u0000${scope}`;
			if (!checked.has(key)) checked.set(key, { role: r.role, path: b.path, name: b.role, scope });
		}
		const ordered = [...checked.values()].sort(
			(a, b) => compare(a.name, b.name) || compare(a.scope, b.scope)
		);
		for (const c of ordered) {
			const left = after.some(
				(b) =>
					b.role === c.name && b.path[b.path.length - 1] === c.scope && this.effective(b) !== null
			);
			if (left) continue;
			const bypassed =
				c.role.protection?.kind === 'bypass' &&
				this.allowed(actor, c.role.protection.permission, c.path);
			if (!bypassed) return { role: c.name, scope: c.scope };
		}
		return null;
	}
}
