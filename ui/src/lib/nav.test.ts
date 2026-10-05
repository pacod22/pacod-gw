import { describe, expect, it } from 'vitest';
import { canAdminOrg, canAdminWorkspace, canWrite, hasPlatformRole } from './access';
import type { Principal } from './api/types';
import { buildNav, isCurrent, scopeQuery, switchTarget } from './nav';

const principal = (roles: Principal['roles'], platform_admin = false): Principal => ({
	id: 'usr_1',
	kind: 'user',
	display: 'someone@example.test',
	roles,
	platform_admin
});

const admin = principal([{ role: 'platform_admin', scope: { type: 'platform' } }], true);
const orgAdmin = principal([{ role: 'org_admin', scope: { type: 'organisation', id: 'org_a' } }]);
const wsViewer = principal([{ role: 'viewer', scope: { type: 'workspace', id: 'ws_1' } }]);
const auditor = principal([{ role: 'auditor', scope: { type: 'platform' } }]);

const labels = (p: Principal, scope = { org: null as string | null, ws: null as string | null }) =>
	Object.fromEntries(buildNav(p, scope).map((g) => [g.label, g.items.map((i) => i.label)]));

describe('buildNav', () => {
	it('groups the pages as the concept does', () => {
		expect(buildNav(admin, { org: 'org_a', ws: 'ws_1' }).map((g) => g.label)).toEqual([
			'Home',
			'Platform',
			'Organisation',
			'Workspace',
			'Insight'
		]);
	});

	it('shows platform pages by role', () => {
		const all = ['Providers', 'Models', 'Prices', 'Organisations', 'Access', 'System', 'Settings'];
		expect(labels(admin).Platform).toEqual(all);
		expect(labels(auditor).Platform).toEqual(all.filter((label) => label !== 'Access'));
		expect(labels(orgAdmin).Platform).toBeUndefined();
		expect(labels(wsViewer).Platform).toBeUndefined();
	});

	it('asks for a selection before it shows organisation and workspace pages', () => {
		const none = buildNav(orgAdmin, { org: null, ws: null });
		expect(none.find((g) => g.label === 'Organisation')?.hint).toMatch(/Choose an organisation/);
		expect(none.find((g) => g.label === 'Workspace')?.hint).toMatch(/Choose a workspace/);

		const selected = buildNav(orgAdmin, { org: 'org_a', ws: 'ws 1' });
		expect(selected.find((g) => g.label === 'Organisation')?.items[0].href).toBe(
			'/orgs/org_a/workspaces'
		);
		expect(selected.find((g) => g.label === 'Organisation')?.items.map((i) => i.label)).toEqual([
			'Workspaces',
			'Model grants',
			'Limits'
		]);
		expect(selected.find((g) => g.label === 'Workspace')?.items.map((i) => i.href)).toEqual([
			'/orgs/org_a/workspaces/ws%201/grants',
			'/orgs/org_a/workspaces/ws%201/templates',
			'/orgs/org_a/workspaces/ws%201/keys',
			'/orgs/org_a/workspaces/ws%201/limits',
			'/orgs/org_a/workspaces/ws%201/settings'
		]);
	});

	it('carries the selection to Insight in the query', () => {
		const insight = buildNav(wsViewer, { org: 'org_a', ws: 'ws_1' }).find(
			(g) => g.label === 'Insight'
		);
		expect(insight?.items[0].href).toBe(
			'/insight/requests?organisation_id=org_a&workspace_id=ws_1'
		);
		expect(scopeQuery({ org: null, ws: 'ws_1' })).toBe('');
	});
});

describe('isCurrent', () => {
	it('marks a page and the pages below it, but not its siblings', () => {
		expect(isCurrent({ label: 'Dashboard', href: '/' }, '/')).toBe(true);
		expect(isCurrent({ label: 'Dashboard', href: '/' }, '/platform/providers')).toBe(false);
		const providers = { label: 'Providers', href: '/platform/providers' };
		expect(isCurrent(providers, '/platform/providers/prv_1/credentials')).toBe(true);
		expect(isCurrent(providers, '/platform/providers-old')).toBe(false);
		const workspaces = { label: 'Workspaces', href: '/orgs/o/workspaces', exact: true };
		expect(isCurrent(workspaces, '/orgs/o/workspaces')).toBe(true);
		expect(isCurrent(workspaces, '/orgs/o/workspaces/w/keys')).toBe(false);
		const requests = {
			label: 'Requests',
			href: '/insight/requests?organisation_id=o',
			match: '/insight/requests'
		};
		expect(isCurrent(requests, '/insight/requests/abc')).toBe(true);
	});
});

describe('switchTarget', () => {
	it('opens the workspaces of a newly chosen organisation', () => {
		expect(
			switchTarget({
				org: 'org_b',
				ws: 'ws_1',
				was: 'org_a',
				from: '/orgs/org_a/workspaces/ws_1/keys'
			})
		).toBe('/orgs/org_b/workspaces');
		expect(switchTarget({ org: 'org_a', ws: '', was: '', from: '/' })).toBe(
			'/orgs/org_a/workspaces'
		);
	});

	it('keeps the kind of page when the workspace changes', () => {
		expect(
			switchTarget({
				org: 'org_a',
				ws: 'ws_2',
				was: 'org_a',
				from: '/orgs/org_a/workspaces/ws_1/settings'
			})
		).toBe('/orgs/org_a/workspaces/ws_2/settings');
		expect(
			switchTarget({ org: 'org_a', ws: 'ws_2', was: 'org_a', from: '/orgs/org_a/workspaces' })
		).toBe('/orgs/org_a/workspaces/ws_2/keys');
	});

	it('keeps Insight pages and changes their scope', () => {
		expect(
			switchTarget({ org: 'org_a', ws: 'ws_2', was: 'org_a', from: '/insight/requests' })
		).toBe('/insight/requests?organisation_id=org_a&workspace_id=ws_2');
		expect(switchTarget({ org: '', ws: '', was: 'org_a', from: '/insight/requests' })).toBe(
			'/insight/requests'
		);
	});

	it('goes home when nothing is selected', () => {
		expect(
			switchTarget({ org: '', ws: 'ws_1', was: 'org_a', from: '/orgs/org_a/workspaces' })
		).toBe('/');
	});
});

describe('what a role may change', () => {
	it('mirrors the core’s role model', () => {
		expect(hasPlatformRole(admin)).toBe(true);
		expect(hasPlatformRole(auditor)).toBe(true);
		expect(hasPlatformRole(orgAdmin)).toBe(false);

		expect(canAdminOrg(orgAdmin, 'org_a')).toBe(true);
		expect(canAdminOrg(orgAdmin, 'org_b')).toBe(false);
		expect(canAdminWorkspace(orgAdmin, 'ws_1', 'org_a')).toBe(true);
		expect(canAdminWorkspace(wsViewer, 'ws_1', 'org_a')).toBe(false);

		const wsAdmin = principal([
			{ role: 'workspace_admin', scope: { type: 'workspace', id: 'ws_1' } }
		]);
		expect(canWrite(wsAdmin, 'workspace', { org: 'org_a', ws: 'ws_1' })).toBe(true);
		expect(canWrite(wsAdmin, 'workspace', { org: 'org_a', ws: 'ws_2' })).toBe(false);
		// The workspace object itself is written at the organisation's level.
		expect(canWrite(wsAdmin, 'organisation', { org: 'org_a' })).toBe(false);
		expect(canWrite(auditor, 'platform', {})).toBe(false);
		expect(canWrite(admin, 'platform', {})).toBe(true);
	});
});
