import { describe, expect, it } from 'vitest';
import { copyTargets, storedKeys, type CopyTarget, type OwnCredential } from './credentials';

function key(id: string, scope: OwnCredential['scope'], more: Partial<OwnCredential> = {}) {
	return {
		id,
		version: 1,
		created_at: '2026-10-18T00:00:00Z',
		updated_at: '2026-10-18T00:00:00Z',
		provider_id: 'prv_1',
		provider_name: 'OpenAI (openai)',
		scope,
		label: '',
		fingerprint: `…${id} (00000000)`,
		state: 'current',
		retire_at: null,
		kek_id: 'k1',
		...more
	} as OwnCredential;
}

const shared = key('shared', { type: 'global' }, { label: 'prod' });
const spare = key('spare', { type: 'global' }, { state: 'retiring' });
const old = key('old', { type: 'global' }, { state: 'disabled' });
const orgA = key('orga', { type: 'organisation', id: 'org_a' });
const orgB = key('orgb', { type: 'organisation', id: 'org_b' });
const wsA = key('wsa', { type: 'workspace', id: 'ws_a' });

describe('storedKeys', () => {
	const all = [shared, spare, old, orgA, orgB, wsA];

	it('offers an organisation the installation’s keys that are not disabled', () => {
		const offered = storedKeys(all, { type: 'organisation', id: 'org_a' }, 'org_a');
		expect(offered.map((o) => o.value)).toEqual(['prv_1/shared', 'prv_1/spare']);
		expect(offered[0].label).toBe('OpenAI (openai): prod — installation’s key, in use');
		expect(offered[0].help).toMatch(/^Fingerprint …shared \(00000000\)\. .*shared key/);
		expect(offered[1].label).toBe(
			'OpenAI (openai): …spare (00000000) — installation’s key, kept back, not in use'
		);
		expect(offered[1].help).not.toContain('shared key');
	});

	it('offers a workspace its own organisation’s keys too, never another’s', () => {
		const offered = storedKeys(all, { type: 'workspace', id: 'ws_a' }, 'org_a');
		expect(offered.map((o) => o.value)).toEqual(['prv_1/shared', 'prv_1/spare', 'prv_1/orga']);
	});

	it('leaves out a key the scope already has', () => {
		const copy = key(
			'copy',
			{ type: 'organisation', id: 'org_a' },
			{ fingerprint: shared.fingerprint }
		);
		const offered = storedKeys([...all, copy], { type: 'organisation', id: 'org_a' }, 'org_a');
		expect(offered.map((o) => o.value)).toEqual(['prv_1/spare']);
		// A disabled copy does not count: the key can be given again.
		const disabled = { ...copy, state: 'disabled' as const };
		const again = storedKeys([...all, disabled], { type: 'organisation', id: 'org_a' }, 'org_a');
		expect(again.map((o) => o.value)).toEqual(['prv_1/shared', 'prv_1/spare']);
	});
});

describe('copyTargets', () => {
	const targets: CopyTarget[] = [
		{ type: 'organisation', id: 'org_a', organisation: 'org_a', label: 'Organisation A' },
		{ type: 'organisation', id: 'org_b', organisation: 'org_b', label: 'Organisation B' },
		{ type: 'workspace', id: 'ws_a', organisation: 'org_a', label: 'Workspace a / one' },
		{ type: 'workspace', id: 'ws_b', organisation: 'org_b', label: 'Workspace b / one' }
	];
	const ids = (list: CopyTarget[]) => list.map((t) => t.id);

	it('gives the installation’s key anywhere it is not yet', () => {
		const copy = key(
			'copy',
			{ type: 'workspace', id: 'ws_b' },
			{ fingerprint: shared.fingerprint }
		);
		expect(ids(copyTargets(shared, targets, [shared, copy]))).toEqual(['org_a', 'org_b', 'ws_a']);
	});

	it('gives an organisation’s key to its own workspaces only', () => {
		expect(ids(copyTargets(orgA, targets, [orgA]))).toEqual(['ws_a']);
	});

	it('gives neither a workspace’s key nor a disabled one', () => {
		expect(copyTargets(wsA, targets, [wsA])).toEqual([]);
		expect(copyTargets(old, targets, [old])).toEqual([]);
	});
});
