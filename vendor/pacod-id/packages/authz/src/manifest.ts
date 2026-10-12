// A product's manifest: its scope types, roles and permissions, read and checked once.
// The checks and their order follow crates/pacod-id-authz/src/manifest.rs and the vectors in
// policy/engine-vectors/.

export const PLATFORM = 'platform';
export const ORGANISATION = 'organisation';
const RESERVED_PRODUCTS = [PLATFORM, ORGANISATION, 'custom'];

/** A manifest as written: local names, as in `authz/<product>.yaml`. */
export interface ManifestSource {
	product: string;
	scope_types?: Record<string, { parent: string }>;
	roles?: Record<string, RoleSource>;
	custom_roles?: { at: string[] };
	grant?: Record<string, string>;
}

export interface RoleSource {
	at: string[];
	permissions?: string[];
	protected?: boolean | { bypass: string };
	implicit?: boolean;
}

export type ManifestErrorCode =
	| 'bad_manifest'
	| 'bad_product'
	| 'bad_name'
	| 'unknown_parent'
	| 'parent_cycle'
	| 'unknown_scope_type'
	| 'bad_permission'
	| 'implicit_not_platform'
	| 'unknown_grant_role'
	| 'missing_grant_default';

/** Why a manifest was refused; `code` is the same in every port of the engine. */
export class ManifestError extends Error {
	constructor(
		readonly code: ManifestErrorCode,
		message: string
	) {
		super(message);
		this.name = 'ManifestError';
	}
}

/** When a protected role's last holder at a scope may still go. */
export type Protection = { kind: 'always' } | { kind: 'bypass'; permission: string };

type Pattern =
	{ kind: 'all' } | { kind: 'prefix'; prefix: string } | { kind: 'exact'; name: string };

/** A role, with qualified names. */
export interface Role {
	/** `<product>.<name>`. */
	readonly name: string;
	/** Qualified scope types the role may be bound at. */
	readonly at: readonly string[];
	readonly protection: Protection | null;
	readonly implicit: boolean;
	/** @internal */
	readonly patterns: readonly Pattern[];
}

export function isIdent(s: string): boolean {
	return /^[a-z][a-z0-9_]*$/.test(s);
}

/** Identifiers joined by dots. */
export function isPermissionName(s: string): boolean {
	return s.length > 0 && s.split('.').every(isIdent);
}

function pattern(s: string): Pattern {
	if (s === '*') return { kind: 'all' };
	if (s.endsWith('.*') && isPermissionName(s.slice(0, -2))) {
		return { kind: 'prefix', prefix: s.slice(0, -1) };
	}
	if (isPermissionName(s)) return { kind: 'exact', name: s };
	throw new ManifestError('bad_permission', `${JSON.stringify(s)} is not a valid permission`);
}

export function patternMatches(p: Pattern, local: string): boolean {
	switch (p.kind) {
		case 'all':
			return true;
		case 'prefix':
			return local.length > p.prefix.length && local.startsWith(p.prefix);
		case 'exact':
			return local === p.name;
	}
}

/** Plain byte-order comparison, the order Rust's BTreeMap and Python's sorted() use. */
export function compare(a: string, b: string): number {
	return a < b ? -1 : a > b ? 1 : 0;
}

const sortedKeys = (o: Record<string, unknown>) => Object.keys(o).sort(compare);

function isObject(v: unknown): v is Record<string, unknown> {
	return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function shape(ok: boolean, what: string): void {
	if (!ok)
		throw new ManifestError(
			'bad_manifest',
			`the manifest does not have the expected shape: ${what}`
		);
}

function onlyKeys(o: Record<string, unknown>, allowed: string[], where: string): void {
	for (const k of Object.keys(o)) shape(allowed.includes(k), `unknown field ${k} in ${where}`);
}

function stringList(v: unknown, what: string): string[] {
	shape(
		Array.isArray(v) && v.every((x) => typeof x === 'string'),
		`${what} must be a list of strings`
	);
	return v as string[];
}

/** Checks the shape of untrusted input, as serde does for the Rust port. */
function readSource(input: unknown): ManifestSource {
	shape(isObject(input), 'not a map');
	const o = input as Record<string, unknown>;
	onlyKeys(o, ['product', 'scope_types', 'roles', 'custom_roles', 'grant'], 'the manifest');
	shape(typeof o.product === 'string', 'product must be a string');
	const source: ManifestSource = { product: o.product as string };
	if (o.scope_types !== undefined) {
		shape(isObject(o.scope_types), 'scope_types must be a map');
		const types: Record<string, { parent: string }> = {};
		for (const [name, st] of Object.entries(o.scope_types as Record<string, unknown>)) {
			shape(isObject(st), `scope type ${name}`);
			const t = st as Record<string, unknown>;
			onlyKeys(t, ['parent'], `scope type ${name}`);
			shape(typeof t.parent === 'string', `scope type ${name} needs a parent`);
			types[name] = { parent: t.parent as string };
		}
		source.scope_types = types;
	}
	if (o.roles !== undefined) {
		shape(isObject(o.roles), 'roles must be a map');
		const roles: Record<string, RoleSource> = {};
		for (const [name, r] of Object.entries(o.roles as Record<string, unknown>)) {
			shape(isObject(r), `role ${name}`);
			const role = r as Record<string, unknown>;
			onlyKeys(role, ['at', 'permissions', 'protected', 'implicit'], `role ${name}`);
			const out: RoleSource = { at: stringList(role.at, `role ${name}: at`) };
			if (role.permissions !== undefined)
				out.permissions = stringList(role.permissions, `role ${name}: permissions`);
			if (role.protected !== undefined) {
				const p = role.protected;
				if (typeof p === 'boolean') out.protected = p;
				else {
					shape(isObject(p), `role ${name}: protected`);
					const pp = p as Record<string, unknown>;
					onlyKeys(pp, ['bypass'], `role ${name}: protected`);
					shape(typeof pp.bypass === 'string', `role ${name}: protected.bypass`);
					out.protected = { bypass: pp.bypass as string };
				}
			}
			if (role.implicit !== undefined) {
				shape(typeof role.implicit === 'boolean', `role ${name}: implicit`);
				out.implicit = role.implicit as boolean;
			}
			roles[name] = out;
		}
		source.roles = roles;
	}
	if (o.custom_roles !== undefined) {
		shape(isObject(o.custom_roles), 'custom_roles must be a map');
		const c = o.custom_roles as Record<string, unknown>;
		onlyKeys(c, ['at'], 'custom_roles');
		source.custom_roles = { at: stringList(c.at, 'custom_roles: at') };
	}
	if (o.grant !== undefined) {
		shape(isObject(o.grant), 'grant must be a map');
		const g = o.grant as Record<string, unknown>;
		for (const [k, v] of Object.entries(g))
			shape(typeof v === 'string', `grant.${k} must be a string`);
		source.grant = g as Record<string, string>;
	}
	return source;
}

/** A checked manifest, with qualified names. */
export class Manifest {
	private constructor(
		readonly product: string,
		/** Qualified scope type → qualified parent type. */
		private readonly scopeParents: ReadonlyMap<string, string>,
		private readonly roleMap: ReadonlyMap<string, Role>,
		/** Qualified scope types custom roles may be bound at; null when custom roles are off. */
		readonly customAt: readonly string[] | null,
		private readonly grant: ReadonlyMap<string, string>,
		private readonly grantDefault: string,
		readonly source: ManifestSource
	) {}

	/**
	 * Checks a manifest and qualifies its names. The order of the checks is part of the
	 * engine's specification: product, scope type names, parents, cycles, roles (sorted by
	 * name), custom roles, grant.
	 */
	static fromSource(input: unknown): Manifest {
		const source = readSource(input);
		const product = source.product;
		if (!isIdent(product) || RESERVED_PRODUCTS.includes(product)) {
			throw new ManifestError(
				'bad_product',
				`${JSON.stringify(product)} is not a valid product name`
			);
		}
		const qualify = (local: string) => `${product}.${local}`;
		const scopeTypes = source.scope_types ?? {};
		for (const name of sortedKeys(scopeTypes)) {
			if (!isIdent(name) || name === PLATFORM || name === ORGANISATION) {
				throw new ManifestError('bad_name', `${JSON.stringify(name)} is not a valid name`);
			}
		}
		const resolveType = (name: string): string | null => {
			if (name === PLATFORM || name === ORGANISATION) return name;
			const local = name.startsWith(`${product}.`) ? name.slice(product.length + 1) : name;
			return Object.hasOwn(scopeTypes, local) ? qualify(local) : null;
		};
		const scopeParents = new Map<string, string>();
		for (const name of sortedKeys(scopeTypes)) {
			const parent = resolveType(scopeTypes[name].parent);
			if (parent === null) {
				throw new ManifestError(
					'unknown_parent',
					`scope type ${name} has an unknown parent ${scopeTypes[name].parent}`
				);
			}
			scopeParents.set(qualify(name), parent);
		}
		for (const start of [...scopeParents.keys()].sort(compare)) {
			const seen = new Set<string>();
			let at: string | undefined = start;
			while (at !== undefined && scopeParents.has(at)) {
				if (seen.has(at))
					throw new ManifestError('parent_cycle', `scope type ${start} is its own ancestor`);
				seen.add(at);
				at = scopeParents.get(at);
			}
		}

		const roles = new Map<string, Role>();
		const roleSources = source.roles ?? {};
		for (const name of sortedKeys(roleSources)) {
			const r = roleSources[name];
			if (!isIdent(name) || name === 'custom') {
				throw new ManifestError('bad_name', `${JSON.stringify(name)} is not a valid name`);
			}
			const at = r.at.map((kind) => {
				const resolved = resolveType(kind);
				if (resolved === null) {
					throw new ManifestError(
						'unknown_scope_type',
						`role ${name} names an unknown scope type ${kind}`
					);
				}
				return resolved;
			});
			const patterns = (r.permissions ?? []).map(pattern);
			let protection: Protection | null = null;
			if (r.protected === true) protection = { kind: 'always' };
			else if (typeof r.protected === 'object') {
				if (!isPermissionName(r.protected.bypass)) {
					throw new ManifestError(
						'bad_permission',
						`${JSON.stringify(r.protected.bypass)} is not a valid permission`
					);
				}
				protection = { kind: 'bypass', permission: qualify(r.protected.bypass) };
			}
			const implicit = r.implicit === true;
			if (implicit && (at.length !== 1 || at[0] !== PLATFORM)) {
				throw new ManifestError(
					'implicit_not_platform',
					`implicit role ${name} must be bound at the platform only`
				);
			}
			roles.set(qualify(name), { name: qualify(name), at, patterns, protection, implicit });
		}

		let customAt: string[] | null = null;
		if (source.custom_roles !== undefined) {
			customAt = source.custom_roles.at.map((kind) => {
				const resolved = resolveType(kind);
				if (resolved === null) {
					throw new ManifestError(
						'unknown_scope_type',
						`custom roles name an unknown scope type ${kind}`
					);
				}
				return resolved;
			});
		}

		const grant = new Map<string, string>();
		let grantDefault: string | null = null;
		const grantSource = source.grant ?? {};
		for (const role of sortedKeys(grantSource)) {
			const permission = grantSource[role];
			if (!isPermissionName(permission)) {
				throw new ManifestError(
					'bad_permission',
					`${JSON.stringify(permission)} is not a valid permission`
				);
			}
			if (role === 'default') grantDefault = qualify(permission);
			else if (Object.hasOwn(roleSources, role)) grant.set(qualify(role), qualify(permission));
			else throw new ManifestError('unknown_grant_role', `grant names an unknown role ${role}`);
		}
		if (grantDefault === null)
			throw new ManifestError('missing_grant_default', 'grant has no default');

		return new Manifest(product, scopeParents, roles, customAt, grant, grantDefault, source);
	}

	/** The roles, sorted by qualified name. */
	roles(): Role[] {
		return [...this.roleMap.values()].sort((a, b) => compare(a.name, b.name));
	}

	role(qualified: string): Role | undefined {
		return this.roleMap.get(qualified);
	}

	/** The qualified scope types the product registers, with their parents. */
	scopeTypes(): [string, string][] {
		return [...this.scopeParents.entries()].sort((a, b) => compare(a[0], b[0]));
	}

	scopeParent(kind: string): string | undefined {
		return this.scopeParents.get(kind);
	}

	/** The qualified permission needed to give or take `role`. */
	grantPermission(role: string): string {
		return this.grant.get(role) ?? this.grantDefault;
	}
}

/** Whether a role grants this local permission name of its product. */
export function grantsLocal(role: Role, local: string): boolean {
	return role.patterns.some((p) => patternMatches(p, local));
}
