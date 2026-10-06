import { describe, expect, it } from 'vitest';
import {
	canAdminOrg,
	canAdminWorkspace,
	canReadOrg,
	canWrite,
	describeRole,
	hasPlatformRole
} from './access';
import type { Principal } from './api/types';
import { buildNav, filterChoices, isCurrent, scopeCrumbs, scopeQuery, switchTarget } from './nav';

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
		const all = [
			'Providers',
			'Models',
			'Prices',
			'Price tracker',
			'All organisations',
			'Access',
			'Testing',
			'System',
			'Settings'
		];
		expect(labels(admin).Platform).toEqual(all);
		// The price tracker, the access pages and testing are for platform administrators.
		const adminOnly = ['Access', 'Price tracker', 'Testing'];
		expect(labels(auditor).Platform).toEqual(all.filter((label) => !adminOnly.includes(label)));
		expect(labels(orgAdmin).Platform).toBeUndefined();
		expect(labels(wsViewer).Platform).toBeUndefined();
	});

	it('asks for a selection before it shows organisation and workspace pages', () => {
		const none = buildNav(orgAdmin, { org: null, ws: null });
		expect(none.find((g) => g.label === 'Organisation')?.hint).toMatch(/Choose an organisation/);
		expect(none.find((g) => g.label === 'Workspace')?.hint).toMatch(/Choose a workspace/);

		const selected = buildNav(orgAdmin, { org: 'org_a', ws: 'ws 1' });
		expect(selected.find((g) => g.label === 'Organisation')?.items[0].href).toBe('/orgs/org_a');
		expect(selected.find((g) => g.label === 'Organisation')?.items.map((i) => i.label)).toEqual([
			'Overview',
			'Workspaces',
			'Members',
			'Model grants',
			'Limits',
			'Routing schemes',
			'Settings'
		]);
		expect(selected.find((g) => g.label === 'Workspace')?.items.map((i) => i.href)).toEqual([
			'/orgs/org_a/workspaces/ws%201',
			'/orgs/org_a/workspaces/ws%201/keys',
			'/orgs/org_a/workspaces/ws%201/templates',
			'/orgs/org_a/workspaces/ws%201/grants',
			'/orgs/org_a/workspaces/ws%201/limits',
			'/orgs/org_a/workspaces/ws%201/members',
			'/orgs/org_a/workspaces/ws%201/playground',
			'/orgs/org_a/workspaces/ws%201/settings'
		]);
	});

	it('carries the selection to Insight in the query', () => {
		const insight = buildNav(wsViewer, { org: 'org_a', ws: 'ws_1' }).find(
			(g) => g.label === 'Insight'
		);
		expect(insight?.items.find((i) => i.label === 'Requests')?.href).toBe(
			'/insight/requests?organisation_id=org_a&workspace_id=ws_1'
		);
		expect(scopeQuery({ org: null, ws: 'ws_1' })).toBe('');
	});

	it('shows audit records to administrators and auditors, not to viewers', () => {
		const everyone = [
			'Analytics',
			'Usage and costs',
			'Breakdown',
			'Ranking',
			'End users',
			'Errors',
			'Requests',
			'Monitoring'
		];
		expect(labels(wsViewer).Insight).toEqual(everyone);
		const withAudit = [...everyone.slice(0, 7), 'Audit records', 'Monitoring'];
		expect(labels(orgAdmin).Insight).toEqual(withAudit);
		// The metrics count every organisation, so only platform roles see them.
		expect(labels(auditor).Insight).toEqual([...withAudit, 'Metrics']);
		expect(labels(admin).Insight).toEqual([...withAudit, 'Metrics']);
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
	it('opens the overview of a newly chosen organisation', () => {
		expect(
			switchTarget({
				org: 'org_b',
				ws: 'ws_1',
				was: 'org_a',
				from: '/orgs/org_a/workspaces/ws_1/keys'
			})
		).toBe('/orgs/org_b');
		expect(switchTarget({ org: 'org_a', ws: '', was: '', from: '/' })).toBe('/orgs/org_a');
		expect(switchTarget({ org: 'org_b', ws: '', was: 'org_a', from: '/orgs/org_a/members' })).toBe(
			'/orgs/org_b/members'
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
		).toBe('/orgs/org_a/workspaces/ws_2');
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
		// Creating workspaces is the organisation's business.
		expect(canWrite(wsAdmin, 'organisation', { org: 'org_a' })).toBe(false);
		expect(canWrite(auditor, 'platform', {})).toBe(false);
		expect(canWrite(admin, 'platform', {})).toBe(true);
	});
});

describe('workspace-only roles', () => {
	it('see the organisation only through their workspaces', () => {
		expect(canReadOrg(wsViewer, 'org_a')).toBe(false);
		expect(canReadOrg(orgAdmin, 'org_a')).toBe(true);
		expect(canReadOrg(auditor, 'org_a')).toBe(true);
		expect(labels(wsViewer, { org: 'org_a', ws: 'ws_1' }).Organisation).toEqual([
			'Overview',
			'Workspaces'
		]);
		expect(labels(orgAdmin, { org: 'org_a', ws: null }).Organisation).toContain('Model grants');
	});
});

describe('describeRole', () => {
	it('names the scope instead of showing its id', () => {
		expect(describeRole(admin.roles[0])).toBe('Platform admin');
		expect(describeRole(auditor.roles[0])).toBe('Auditor of the platform');
		expect(
			describeRole({
				role: 'org_admin',
				scope: { type: 'organisation', id: 'org_a' },
				scope_name: 'Acme',
				organisation_id: 'org_a',
				organisation_name: 'Acme'
			})
		).toBe('Organisation admin of organisation Acme');
		expect(
			describeRole({
				role: 'viewer',
				scope: { type: 'workspace', id: 'ws_1' },
				scope_name: 'Billing',
				organisation_id: 'org_a',
				organisation_name: 'Acme'
			})
		).toBe('Viewer of workspace Billing (Acme)');
		// Without names, the id is what there is.
		expect(describeRole(wsViewer.roles[0])).toBe('Viewer of workspace ws_1');
	});
});

describe('filterChoices', () => {
	const choices = [
		{ id: '1', name: 'billing-eu', label: 'Billing Europe' },
		{ id: '2', name: 'research', label: 'Research & Développement' },
		{ id: '3', name: 'eu-ops', label: 'Operations' }
	];
	const ids = (q: string) => filterChoices(choices, q).map((c) => c.id);

	it('matches display names and slugs, without case or accents', () => {
		expect(ids('')).toEqual(['1', '2', '3']);
		expect(ids('develop')).toEqual(['2']);
		expect(ids('OPS')).toEqual(['3']);
		expect(ids('bill eu')).toEqual(['1']);
		expect(ids('nothing')).toEqual([]);
	});

	it('puts display names that start with the query first', () => {
		expect(ids('eu')).toEqual(['1', '3']);
		expect(ids('o')).toEqual(['3', '1', '2']);
	});
});

describe('scopeCrumbs', () => {
	const orgs = [{ id: 'org_a', label: 'Acme' }];
	const workspaces = [{ id: 'ws_1', label: 'Billing' }];
	const crumbs = (path: string) => scopeCrumbs(path, orgs, workspaces).map((c) => c.label);

	it('names the organisation and the workspace above a page', () => {
		expect(crumbs('/orgs/org_a')).toEqual([]);
		expect(crumbs('/orgs/org_a/members')).toEqual(['Acme']);
		expect(crumbs('/orgs/org_a/workspaces/ws_1')).toEqual(['Acme']);
		expect(crumbs('/orgs/org_a/workspaces/ws_1/keys')).toEqual(['Acme', 'Billing']);
		expect(crumbs('/orgs/org_a/workspaces/ws_1/templates/tpl_1')).toEqual(['Acme', 'Billing']);
		expect(scopeCrumbs('/orgs/org_a/workspaces/ws_1/keys', orgs, workspaces)[1].href).toBe(
			'/orgs/org_a/workspaces/ws_1'
		);
		expect(crumbs('/platform/providers')).toEqual([]);
	});

	it('does without names it does not know', () => {
		expect(crumbs('/orgs/org_x/workspaces/ws_9/keys')).toEqual(['Organisation', 'Workspace']);
	});
});
