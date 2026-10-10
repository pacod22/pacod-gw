import { describe, expect, it } from 'vitest';
import {
	canAdminOrg,
	canAdminWorkspace,
	canReadOrg,
	canWrite,
	describeRole,
	hasPlatformRole,
	type Scope
} from './access';
import type { Principal } from './api/types';
import {
	buildNav,
	carryQuery,
	currentTab,
	filterChoices,
	insightSections,
	insightTabs,
	isCurrent,
	levelOf,
	pageScope,
	requestTabs,
	scopeQuery,
	scopeTrail,
	switchTarget,
	type Nav
} from './nav';

const principal = (roles: Principal['roles'], platform_admin = false): Principal => ({
	id: 'usr_1',
	kind: 'user',
	display: 'someone@example.test',
	roles,
	platform_admin,
	reporting_currency: 'USD'
});

const admin = principal([{ role: 'platform_admin', scope: { type: 'platform' } }], true);
const orgAdmin = principal([{ role: 'org_admin', scope: { type: 'organisation', id: 'org_a' } }]);
const wsViewer = principal([{ role: 'viewer', scope: { type: 'workspace', id: 'ws_1' } }]);
const auditor = principal([{ role: 'auditor', scope: { type: 'platform' } }]);

const top = { org: null, ws: null };
const inOrg = { org: 'org_a', ws: null };
const inWs = { org: 'org_a', ws: 'ws_1' };
const oneOrg = [{ id: 'org_a', label: 'Acme' }];

const nav = (p: Principal, scope: Scope = top, orgs = oneOrg) => buildNav(p, scope, orgs);
const links = (n: Nav) => n.groups.flatMap((g) => g.items);
const labels = (p: Principal, scope: Scope = top, orgs = oneOrg) =>
	links(nav(p, scope, orgs)).map((i) => i.label);

describe('buildNav', () => {
	it('lists the pages of the installation at the top, by role', () => {
		expect(nav(admin).groups.map((g) => g.label)).toEqual([
			undefined,
			'Gateway',
			'Tenants',
			'Operations',
			'Insight'
		]);
		expect(labels(admin)).toEqual([
			'Dashboard',
			'Providers',
			'Models',
			'Prices',
			'Organisations',
			'Access',
			'Rate classes',
			'System',
			'Testing',
			'Settings',
			'Usage',
			'Requests',
			'Monitoring'
		]);
		// Access and testing are for platform administrators.
		expect(labels(auditor)).toEqual([
			'Dashboard',
			'Providers',
			'Models',
			'Prices',
			'Organisations',
			'System',
			'Settings',
			'Usage',
			'Requests',
			'Monitoring'
		]);
		// An auditor's System is the audit log, and stays current on the other system pages.
		const system = links(nav(auditor)).find((i) => i.label === 'System');
		expect(system?.href).toBe('/platform/system/audit-log');
		expect(system?.match).toBe('/platform/system');
	});

	it('leads someone without a platform role to their organisations', () => {
		expect(labels(orgAdmin)).toEqual(['Dashboard', 'Acme', 'Usage', 'Requests', 'Monitoring']);
		expect(links(nav(orgAdmin))[1].href).toBe('/orgs/org_a');
		expect(labels(wsViewer, top, [])).toEqual(['Dashboard', 'Usage', 'Requests', 'Monitoring']);
		const many = Array.from({ length: 10 }, (_, i) => ({ id: `org_${i}`, label: `Org ${i}` }));
		const listed = links(nav(wsViewer, top, many));
		expect(listed).toHaveLength(12);
		expect(listed[8]).toMatchObject({
			label: 'All 10 organisations',
			href: '/#your-organisations'
		});
	});

	it('lists the pages of an organisation', () => {
		const pages = [
			'Overview',
			'Workspaces',
			'Members',
			'Model grants',
			'Limits',
			'Routing schemes',
			'Usage',
			'Requests',
			'Settings'
		];
		expect(labels(orgAdmin, inOrg)).toEqual(pages);
		expect(labels(admin, inOrg)).toEqual(pages);
		expect(labels(auditor, inOrg)).toEqual(pages);
		expect(links(nav(orgAdmin, inOrg))[0].href).toBe('/orgs/org_a');
	});

	it('lists the pages of a workspace', () => {
		expect(links(nav(wsViewer, { org: 'org_a', ws: 'ws 1' })).map((i) => i.href)).toEqual([
			'/orgs/org_a/workspaces/ws%201',
			'/orgs/org_a/workspaces/ws%201/members',
			'/orgs/org_a/workspaces/ws%201/keys',
			'/orgs/org_a/workspaces/ws%201/templates',
			'/orgs/org_a/workspaces/ws%201/playground',
			'/orgs/org_a/workspaces/ws%201/files',
			'/orgs/org_a/workspaces/ws%201/grants',
			'/orgs/org_a/workspaces/ws%201/limits',
			'/insight/usage?organisation_id=org_a&workspace_id=ws+1',
			'/insight/requests?organisation_id=org_a&workspace_id=ws+1',
			'/orgs/org_a/workspaces/ws%201/settings'
		]);
		expect(labels(admin, inWs)).toEqual(labels(wsViewer, inWs));
	});

	it('carries the selection to Insight in the query', () => {
		const requests = links(nav(wsViewer, inWs)).find((i) => i.label === 'Requests');
		expect(requests?.href).toBe('/insight/requests?organisation_id=org_a&workspace_id=ws_1');
		expect(scopeQuery({ org: null, ws: 'ws_1' })).toBe('');
	});

	it('keeps every level short, and each label and group once', () => {
		const counts: Record<string, number[]> = {};
		for (const [name, p] of Object.entries({ admin, auditor, orgAdmin, wsViewer })) {
			counts[name] = [top, inOrg, inWs].map((scope) => {
				const n = nav(p, scope);
				const shown = links(n).map((i) => i.label);
				expect(new Set(shown).size).toBe(shown.length);
				expect(new Set(n.groups.map((g) => g.id)).size).toBe(n.groups.length);
				if (scope !== top) {
					// Nothing of the installation leaks into an organisation or a workspace.
					expect(links(n).some((i) => i.href.startsWith('/platform'))).toBe(false);
				}
				return shown.length;
			});
		}
		expect(counts).toEqual({
			admin: [13, 9, 11],
			auditor: [10, 9, 11],
			orgAdmin: [5, 9, 11],
			wsViewer: [5, 4, 11]
		});
	});

	it('marks the entry of every page in its section', () => {
		const current = (path: string) =>
			links(nav(admin))
				.filter((i) => isCurrent(i, path))
				.map((i) => i.label);
		expect(current('/insight/analytics')).toEqual(['Usage']);
		expect(current('/insight/end-users/u_1')).toEqual(['Usage']);
		expect(current('/insight/errors')).toEqual(['Requests']);
		expect(current('/insight/traces/t_1')).toEqual(['Requests']);
		expect(current('/insight/metrics/overview')).toEqual(['Monitoring']);
		expect(current('/insight/usage-old')).toEqual([]);
		expect(current('/platform/price-tracker')).toEqual(['Prices']);
		expect(current('/platform/system/config')).toEqual(['System']);
	});
});

describe('isCurrent', () => {
	it('marks a page and the pages below it, but not its siblings', () => {
		expect(isCurrent({ href: '/' }, '/')).toBe(true);
		expect(isCurrent({ href: '/' }, '/platform/providers')).toBe(false);
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

	it('opens the list of a record, whose scope is its own', () => {
		expect(switchTarget({ org: 'org_b', ws: '', was: 'org_a', from: '/insight/traces/t_1' })).toBe(
			'/insight/requests?organisation_id=org_b'
		);
		expect(
			switchTarget({ org: 'org_a', ws: '', was: 'org_a', from: '/insight/requests/abc/audit' })
		).toBe('/insight/requests?organisation_id=org_a');
		expect(switchTarget({ org: 'org_a', ws: '', was: '', from: '/insight/end-users/u_1' })).toBe(
			'/insight/end-users?organisation_id=org_a'
		);
	});

	it('leaves the pages of the whole installation for the organisation', () => {
		expect(switchTarget({ org: 'org_a', ws: '', was: '', from: '/insight/monitoring' })).toBe(
			'/orgs/org_a'
		);
		expect(
			switchTarget({ org: 'org_a', ws: '', was: '', from: '/insight/metrics/workspaces' })
		).toBe('/orgs/org_a');
		expect(switchTarget({ org: 'org_a', ws: '', was: '', from: '/platform/providers' })).toBe(
			'/orgs/org_a'
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
		expect(labels(wsViewer, inOrg)).toEqual(['Overview', 'Workspaces', 'Usage', 'Requests']);
		expect(labels(orgAdmin, inOrg)).toContain('Model grants');
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

describe('levels', () => {
	const none = { org: null, ws: null };
	const org = { org: 'org_a', ws: null };
	const ws = { org: 'org_a', ws: 'ws_1' };

	it('names the level of a scope', () => {
		expect(levelOf(none)).toBe('top');
		expect(levelOf(org)).toBe('organisation');
		expect(levelOf(ws)).toBe('workspace');
	});

	it('takes the scope only from pages that serve one', () => {
		expect(pageScope('/orgs/org_a/members', org)).toEqual(org);
		expect(pageScope('/', org)).toEqual(org);
		expect(pageScope('/insight/usage', ws)).toEqual(ws);
		expect(pageScope('/insight/requests/abc', org)).toEqual(org);
		// An organisation in the query is a filter there, not a level.
		expect(pageScope('/platform/system/audit-log', org)).toEqual(none);
		expect(pageScope('/insight/metrics/overview', org)).toEqual(none);
		expect(pageScope('/insight/monitoring', org)).toEqual(none);
		expect(pageScope('/account/password', org)).toEqual(none);
	});
});

describe('tabs', () => {
	const testing = [
		{ label: 'Run', href: '/platform/testing' },
		{ label: 'Message', href: '/platform/testing/message', match: '/platform/testing/message' },
		{
			label: 'History',
			href: '/platform/testing/history',
			match: ['/platform/testing/history', '/platform/testing/runs']
		}
	];

	it('marks the tab of a page and of the records below it', () => {
		expect(currentTab(testing, '/platform/testing')).toBe(0);
		expect(currentTab(testing, '/platform/testing/message/m_1')).toBe(1);
		expect(currentTab(testing, '/platform/testing/runs/r_1')).toBe(2);
		expect(currentTab(testing, '/platform/testing/cases')).toBe(-1);
		const users = [
			{ label: 'Users', href: '/platform/access/users', match: '/platform/access/users' }
		];
		expect(currentTab(users, '/platform/access/users/usr_1')).toBe(0);
		expect(currentTab(users, '/platform/access/users-old')).toBe(-1);
	});

	it('passes the named parameters on, and only those', () => {
		const from = new URLSearchParams('organisation_id=org_a&workspace_id=ws_1&page=2');
		const names = ['organisation_id', 'workspace_id'];
		expect(carryQuery('/insight/requests/abc', from, names)).toBe(
			'/insight/requests/abc?organisation_id=org_a&workspace_id=ws_1'
		);
		expect(carryQuery('/x?organisation_id=org_b', from, names)).toBe(
			'/x?organisation_id=org_b&workspace_id=ws_1'
		);
		expect(carryQuery('/x', new URLSearchParams(), names)).toBe('/x');
	});
});

describe('insightSections', () => {
	const tabs = (p: Principal, scope = { org: null as string | null, ws: null as string | null }) =>
		Object.fromEntries(insightSections(p, scope).map((s) => [s.id, s.tabs.map((t) => t.label)]));
	const usage = ['Usage and costs', 'Analytics', 'Breakdown', 'Ranking', 'End users'];

	it('groups the pages and gates them as the roles do', () => {
		expect(tabs(wsViewer)).toEqual({
			usage,
			requests: ['Requests', 'Errors'],
			monitoring: ['Live']
		});
		expect(tabs(orgAdmin).requests).toEqual(['Requests', 'Errors', 'Audit records']);
		expect(tabs(auditor).monitoring).toEqual(['Live', 'Metrics']);
		expect(tabs(admin).monitoring).toEqual(['Live', 'Metrics']);
	});

	it('keeps monitoring for the whole installation and carries the scope', () => {
		const scoped = insightSections(admin, { org: 'org_a', ws: 'ws_1' });
		expect(scoped.map((s) => s.id)).toEqual(['usage', 'requests']);
		expect(scoped[1].tabs[1].href).toBe('/insight/errors?organisation_id=org_a&workspace_id=ws_1');
	});

	it('finds the section and the tab of a page', () => {
		const at = (p: Principal, path: string) => {
			const section = insightTabs(p, { org: null, ws: null }, path);
			return section ? [section.id, currentTab(section.tabs, path)] : null;
		};
		expect(at(orgAdmin, '/insight/requests/abc/audit')).toEqual(['requests', 0]);
		expect(at(orgAdmin, '/insight/traces/t_1')).toEqual(['requests', 0]);
		expect(at(orgAdmin, '/insight/audit-records')).toEqual(['requests', 2]);
		expect(at(wsViewer, '/insight/end-users/u_1')).toEqual(['usage', 4]);
		expect(at(admin, '/insight/metrics/overview')).toEqual(['monitoring', 1]);
		// A section with one page needs no tabs.
		expect(at(wsViewer, '/insight/monitoring')).toBeNull();
		expect(at(admin, '/platform/providers')).toBeNull();
	});

	it('offers the audit record of a call to those who may read it', () => {
		expect(requestTabs('a b', true).map((t) => t.href)).toEqual([
			'/insight/requests/a%20b',
			'/insight/requests/a%20b/audit'
		]);
		expect(requestTabs('abc', false)).toHaveLength(1);
	});
});

describe('scopeTrail', () => {
	const orgs = [{ id: 'org_a', label: 'Acme' }];
	const workspaces = [{ id: 'ws_1', label: 'Billing' }];

	it('leads from the top down to the scope, each step to its home', () => {
		expect(scopeTrail('Platform', { org: null, ws: null }, orgs, workspaces)).toEqual([
			{ level: 'top', label: 'Platform', href: '/' }
		]);
		expect(scopeTrail('Home', { org: 'org_a', ws: 'ws_1' }, orgs, workspaces)).toEqual([
			{ level: 'top', label: 'Home', href: '/' },
			{ level: 'organisation', label: 'Acme', href: '/orgs/org_a' },
			{ level: 'workspace', label: 'Billing', href: '/orgs/org_a/workspaces/ws_1' }
		]);
	});

	it('shows the id of what it cannot name', () => {
		const trail = scopeTrail('Home', { org: 'org_x', ws: 'ws_9' }, orgs, workspaces);
		expect(trail.map((s) => s.label)).toEqual(['Home', 'org_x', 'ws_9']);
	});
});
