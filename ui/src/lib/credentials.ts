/**
 * Which stored provider keys can be given to which organisations and workspaces, as the API's
 * copy (`providers/{id}/credentials/{cid}/copy`) allows: a key of the installation goes to any
 * organisation or workspace, a key of an organisation to its own workspaces. Places that
 * already have the same key are not offered it again.
 */
import type { Credential } from '#lib/api/types.ts';
import type { Option } from '#lib/schema/form.ts';

export type OwnScope = { type: 'organisation'; id: string } | { type: 'workspace'; id: string };

export type OwnCredential = Credential & { provider_name: string };

/** An organisation or a workspace a key can be given to. */
export interface CopyTarget {
	type: OwnScope['type'];
	id: string;
	/** The organisation it is or belongs to. */
	organisation: string;
	label: string;
}

type Keyed = Pick<Credential, 'provider_id' | 'fingerprint' | 'scope' | 'state'>;

function isIn(c: Keyed, scope: OwnScope): boolean {
	return c.scope.type === scope.type && 'id' in c.scope && c.scope.id === scope.id;
}

/** Whether `scope` holds a key with the same secret at the same provider, not disabled. */
function holds(all: Keyed[], scope: OwnScope, key: Keyed): boolean {
	return all.some(
		(c) =>
			c.state !== 'disabled' &&
			c.provider_id === key.provider_id &&
			c.fingerprint === key.fingerprint &&
			isIn(c, scope)
	);
}

/** Whether a key of scope `from` may be given to `to`; `organisation` is `to`'s organisation. */
function flowsDown(from: Credential['scope'], to: OwnScope, organisation: string): boolean {
	if (from.type === 'global') return true;
	return from.type === 'organisation' && to.type === 'workspace' && from.id === organisation;
}

/** How a stored key reads in the list to choose from: provider, name, and where it is kept. */
function storedKeyOption(c: OwnCredential, noun: OwnScope['type']): Option {
	const owner = c.scope.type === 'global' ? 'installation' : 'organisation';
	const use = c.state === 'current' ? 'in use' : c.retire_at ? 'retiring' : 'kept back, not in use';
	const shared =
		c.scope.type === 'global' && c.state === 'current'
			? ` This is the installation’s shared key. As the ${noun}’s own key it lets the ${noun}’s calls reach files, batches and the blind relay on the provider account that every workspace shares.`
			: '';
	return {
		value: `${c.provider_id}/${c.id}`,
		label: `${c.provider_name}: ${c.label || c.fingerprint} — ${owner}’s key, ${use}`,
		help: `Fingerprint ${c.fingerprint}. The ${noun} gets a copy as its own key.${shared}`
	};
}

/**
 * The stored keys `scope` may be given, as choices whose value is `provider id/credential id`.
 * `organisation` is the scope's organisation. `all` is every key the caller may manage.
 */
export function storedKeys(all: OwnCredential[], scope: OwnScope, organisation: string): Option[] {
	return all
		.filter(
			(c) =>
				c.state !== 'disabled' &&
				!isIn(c, scope) &&
				flowsDown(c.scope, scope, organisation) &&
				!holds(all, scope, c)
		)
		.map((c) => storedKeyOption(c, scope.type));
}

/** Where `key` can be given, of `targets`; `all` is every key of its provider. */
export function copyTargets(key: Keyed, targets: CopyTarget[], all: Keyed[]): CopyTarget[] {
	if (key.state === 'disabled') return [];
	return targets.filter(
		(t) =>
			!isIn(key, t) &&
			flowsDown(key.scope, t, t.organisation) &&
			!holds(all, { type: t.type, id: t.id }, key)
	);
}
