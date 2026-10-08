/**
 * The navigation of the console (concept section 15.2): one sidebar per level, reduced to
 * what the principal's roles allow. Organisation and workspace pages carry the selection in
 * the path; Insight pages carry it in the query, so one page serves every scope.
 */
import {
	canReadAuditLog,
	canReadAuditRecords,
	canReadOrg,
	hasPlatformRole,
	type Scope
} from './access';
import type { Principal } from './api/types';

export interface NavItem {
	label: string;
	href: string;
	icon: NavIcon;
	/** Path prefixes that mark the item as current; defaults to the path of `href`. */
	match?: string | readonly string[];
	/** Current only on exactly this path, not on the pages below it. */
	exact?: boolean;
}

export interface NavGroup {
	/** Unique within a level. */
	id: string;
	/** The heading; none for the first group and for a closing one. */
	label?: string;
	items: NavItem[];
}

/** The sidebar of one level. */
export interface Nav {
	level: Level;
	groups: NavGroup[];
}

/** The query that carries the selected scope to pages that are not below `/orgs`. */
export function scopeQuery(scope: Scope): string {
	const query = new URLSearchParams();
	if (scope.org) query.set('organisation_id', scope.org);
	if (scope.org && scope.ws) query.set('workspace_id', scope.ws);
	const text = query.toString();
	return text ? `?${text}` : '';
}

/** The query parameters that carry the scope; tabs pass them on. */
export const SCOPE_PARAMS = ['organisation_id', 'workspace_id'] as const;

/** The level the console shows: the whole installation, one organisation or one workspace. */
export type Level = 'top' | 'organisation' | 'workspace';

export function levelOf(scope: Scope): Level {
	if (scope.org && scope.ws) return 'workspace';
	if (scope.org) return 'organisation';
	return 'top';
}

/**
 * Insight pages that belong to the whole installation: their live figures cover every
 * instance, and an organisation in their query is a filter, not a level.
 */
const INSTALLATION_INSIGHT = ['/insight/monitoring', '/insight/metrics'];

/**
 * The scope of the page shown: the path's below `/orgs`, the query's on the dashboard and
 * the Insight pages that serve every scope, none elsewhere. Other pages, such as the audit
 * log, read `organisation_id` as a filter of their own.
 */
export function pageScope(pathname: string, scope: Scope): Scope {
	if (pathname.startsWith('/orgs/') || pathname === '/') return scope;
	if (
		pathname.startsWith('/insight/') &&
		!INSTALLATION_INSIGHT.some((prefix) => isBelow(pathname, prefix))
	) {
		return scope;
	}
	return { org: null, ws: null };
}

/** Whether `pathname` is `prefix` or a page below it. */
export function isBelow(pathname: string, prefix: string): boolean {
	return pathname === prefix || pathname.startsWith(`${prefix}/`);
}

/** A tab between the sub-pages of one area. */
export interface Tab {
	label: string;
	href: string;
	/** Path prefixes that mark the tab as current; without it, only the path of `href`. */
	match?: string | readonly string[];
}

/** The index of the tab that points at the page shown, or -1; the first that matches wins. */
export function currentTab(tabs: readonly Tab[], pathname: string): number {
	return tabs.findIndex((tab) =>
		tab.match === undefined
			? pathname === tab.href.split('?')[0]
			: [tab.match].flat().some((prefix) => isBelow(pathname, prefix))
	);
}

/** `href` with the named parameters of `from` added where it has none of its own. */
export function carryQuery(
	href: string,
	from: { get(name: string): string | null },
	names: readonly string[]
): string {
	const [path, search = ''] = href.split('?');
	const query = new URLSearchParams(search);
	for (const name of names) {
		const value = from.get(name);
		if (value && !query.has(name)) query.set(name, value);
	}
	const text = query.toString();
	return text ? `${path}?${text}` : path;
}

/** The icon of a navigation item, by meaning; nav-icons.ts maps it to a drawing. */
export type NavIcon =
	| 'dashboard'
	| 'providers'
	| 'models'
	| 'prices'
	| 'organisations'
	| 'organisation'
	| 'access'
	| 'system'
	| 'testing'
	| 'platform-settings'
	| 'overview'
	| 'workspaces'
	| 'members'
	| 'grants'
	| 'limits'
	| 'routing'
	| 'settings'
	| 'keys'
	| 'templates'
	| 'playground'
	| 'usage'
	| 'requests'
	| 'monitoring';

/** One Insight entry of the sidebar and the tabs between its pages. */
export interface InsightSection {
	id: 'usage' | 'requests' | 'monitoring';
	label: string;
	icon: NavIcon;
	tabs: Tab[];
}

/**
 * Insight, grouped: what was used and what it cost, the calls themselves, and the live state
 * of the gateways. Usage and Requests serve every scope and carry it in the query;
 * Monitoring covers every instance, so it belongs to the installation only.
 */
export function insightSections(principal: Principal, scope: Scope): InsightSection[] {
	const query = scopeQuery(scope);
	const tab = (label: string, page: string, match?: string[]): Tab => ({
		label,
		href: `/insight/${page}${query}`,
		match: match ?? `/insight/${page}`
	});
	const sections: InsightSection[] = [
		{
			id: 'usage',
			label: 'Usage',
			icon: 'usage',
			tabs: [
				tab('Usage and costs', 'usage'),
				tab('Analytics', 'analytics'),
				tab('Breakdown', 'breakdown'),
				tab('Ranking', 'ranking'),
				tab('End users', 'end-users')
			]
		},
		{
			id: 'requests',
			label: 'Requests',
			icon: 'requests',
			tabs: [
				tab('Requests', 'requests', ['/insight/requests', '/insight/traces']),
				tab('Errors', 'errors'),
				// Audit records hold prompts and answers: not for viewers.
				...(canReadAuditRecords(principal) ? [tab('Audit records', 'audit-records')] : [])
			]
		}
	];
	if (levelOf(scope) === 'top') {
		sections.push({
			id: 'monitoring',
			label: 'Monitoring',
			icon: 'monitoring',
			tabs: [
				{ label: 'Live', href: '/insight/monitoring', match: '/insight/monitoring' },
				// Prometheus counts every organisation alike.
				...(hasPlatformRole(principal)
					? [{ label: 'Metrics', href: '/insight/metrics', match: '/insight/metrics' }]
					: [])
			]
		});
	}
	return sections;
}

/** The Insight section of the page shown, when it has tabs to choose between. */
export function insightTabs(
	principal: Principal,
	scope: Scope,
	pathname: string
): InsightSection | null {
	const section = insightSections(principal, scope).find((s) => currentTab(s.tabs, pathname) >= 0);
	return section && section.tabs.length > 1 ? section : null;
}

/** Prices and the job that keeps them current; the tracker is for platform administrators. */
export const PRICING_TABS: Tab[] = [
	{ label: 'Prices', href: '/platform/prices' },
	{ label: 'Price tracker', href: '/platform/price-tracker' }
];

/** The views of one call: the call, and its audit record for those who may read it. */
export function requestTabs(id: string, mayReadAudit: boolean): Tab[] {
	const base = `/insight/requests/${encodeURIComponent(id)}`;
	return [
		{ label: 'Call', href: base },
		...(mayReadAudit ? [{ label: 'Audit record', href: `${base}/audit` }] : [])
	];
}

/** A page of a workspace; without `page`, its overview. */
export function workspacePath(org: string, ws: string, page = ''): string {
	const base = `/orgs/${encodeURIComponent(org)}/workspaces/${encodeURIComponent(ws)}`;
	return page ? `${base}/${page}` : base;
}

/** A page of an organisation; without `page`, its overview. */
export function organisationPath(org: string, page = ''): string {
	const base = `/orgs/${encodeURIComponent(org)}`;
	return page ? `${base}/${page}` : base;
}

/** How many organisations the sidebar lists for someone without a platform role. */
const LISTED_ORGS = 8;

/**
 * The sidebar of one level: the installation, an organisation or a workspace. Only the pages
 * of the level shown are listed, so no label is there twice; the scope bar above leads up and
 * across.
 */
export function buildNav(
	principal: Principal,
	scope: Scope,
	orgs: readonly { id: string; label: string }[] = []
): Nav {
	const level = levelOf(scope);
	const insight: NavGroup = {
		id: 'insight',
		label: 'Insight',
		items: insightSections(principal, scope).map((section) => ({
			label: section.label,
			href: section.tabs[0].href,
			icon: section.icon,
			match: section.tabs.flatMap((tab) => [tab.match ?? tab.href.split('?')[0]].flat())
		}))
	};

	if (scope.org && scope.ws) {
		return { level, groups: workspaceGroups(scope.org, scope.ws, insight) };
	}
	if (scope.org) {
		return { level, groups: organisationGroups(principal, scope.org, insight) };
	}

	const dashboard: NavGroup = {
		id: 'main',
		items: [{ label: 'Dashboard', href: '/', icon: 'dashboard' }]
	};

	if (!hasPlatformRole(principal)) {
		// Without a platform role, the way down is through one's own organisations.
		const listed =
			orgs.length > LISTED_ORGS
				? [
						...orgs.slice(0, LISTED_ORGS - 1).map(orgItem),
						{
							label: `All ${orgs.length} organisations`,
							href: '/#your-organisations',
							icon: 'organisations' as const
						}
					]
				: orgs.map(orgItem);
		return {
			level,
			groups: [
				dashboard,
				...(listed.length > 0 ? [{ id: 'orgs', label: 'Your organisations', items: listed }] : []),
				insight
			]
		};
	}

	const tenants: NavItem[] = [
		{ label: 'Organisations', href: '/platform/organisations', icon: 'organisations' }
	];
	if (principal.platform_admin) {
		tenants.push({ label: 'Access', href: '/platform/access', icon: 'access' });
	}
	const operations: NavItem[] = [];
	if (principal.platform_admin || canReadAuditLog(principal)) {
		operations.push({
			label: 'System',
			href: principal.platform_admin ? '/platform/system' : '/platform/system/audit-log',
			icon: 'system',
			match: '/platform/system'
		});
	}
	// Test runs act with the admin's session and create objects: platform admins only.
	if (principal.platform_admin) {
		operations.push({ label: 'Testing', href: '/platform/testing', icon: 'testing' });
	}
	operations.push({ label: 'Settings', href: '/platform/settings', icon: 'platform-settings' });

	return {
		level,
		groups: [
			dashboard,
			{
				id: 'gateway',
				label: 'Gateway',
				items: [
					{ label: 'Providers', href: '/platform/providers', icon: 'providers' },
					{ label: 'Models', href: '/platform/models', icon: 'models' },
					{
						label: 'Prices',
						href: '/platform/prices',
						icon: 'prices',
						match: ['/platform/prices', '/platform/price-tracker']
					}
				]
			},
			{ id: 'tenants', label: 'Tenants', items: tenants },
			{ id: 'operations', label: 'Operations', items: operations },
			insight
		]
	};
}

function workspaceGroups(org: string, ws: string, insight: NavGroup): NavGroup[] {
	const page = (label: string, path: string, icon: NavIcon): NavItem => ({
		label,
		href: workspacePath(org, ws, path),
		icon
	});
	return [
		{
			id: 'main',
			items: [
				{ ...page('Overview', '', 'overview'), exact: true },
				page('Members', 'members', 'members')
			]
		},
		{
			id: 'build',
			label: 'Build',
			items: [
				page('API keys', 'keys', 'keys'),
				page('Templates', 'templates', 'templates'),
				page('Playground', 'playground', 'playground')
			]
		},
		{
			id: 'rules',
			label: 'Rules',
			items: [page('Model grants', 'grants', 'grants'), page('Limits', 'limits', 'limits')]
		},
		insight,
		{ id: 'end', items: [page('Settings', 'settings', 'settings')] }
	];
}

function organisationGroups(principal: Principal, org: string, insight: NavGroup): NavGroup[] {
	const page = (label: string, path: string, icon: NavIcon): NavItem => ({
		label,
		href: organisationPath(org, path),
		icon
	});
	const overview = [
		{ ...page('Overview', '', 'overview'), exact: true },
		{ ...page('Workspaces', 'workspaces', 'workspaces'), exact: true }
	];
	// Someone with a workspace role only sees the organisation's workspaces.
	if (!canReadOrg(principal, org)) return [{ id: 'main', items: overview }, insight];
	return [
		{ id: 'main', items: [...overview, page('Members', 'members', 'members')] },
		{
			id: 'rules',
			label: 'Rules',
			items: [
				page('Model grants', 'grants', 'grants'),
				page('Limits', 'limits', 'limits'),
				page('Routing schemes', 'routing', 'routing')
			]
		},
		insight,
		{ id: 'end', items: [page('Settings', 'settings', 'settings')] }
	];
}

function orgItem(org: { id: string; label: string }): NavItem {
	return { label: org.label, href: organisationPath(org.id), icon: 'organisation' };
}

/** Whether a navigation item points at the page that is shown, or at one above it. */
export function isCurrent(
	item: Pick<NavItem, 'href' | 'match' | 'exact'>,
	pathname: string
): boolean {
	const targets = item.match === undefined ? [item.href.split('?')[0]] : [item.match].flat();
	return targets.some((target) =>
		target === '/' || item.exact ? pathname === target : isBelow(pathname, target)
	);
}

/** The pages below a workspace that stay open while the switcher changes the workspace. */
const WORKSPACE_PAGES = new Set([
	'grants',
	'templates',
	'keys',
	'limits',
	'members',
	'playground',
	'settings'
]);
/** The pages below an organisation that stay open while the switcher changes the organisation. */
const ORGANISATION_PAGES = new Set([
	'workspaces',
	'members',
	'grants',
	'limits',
	'routing',
	'settings'
]);

export interface SwitchInput {
	/** The organisation that was chosen; empty for none. */
	org: string;
	/** The workspace that was chosen; empty for none. */
	ws: string;
	/** The organisation that was selected before. */
	was: string;
	/** The path of the page the switcher was used on. */
	from: string;
}

/**
 * Where the organisation and workspace switcher leads. The selection lives in the URL, so
 * switching is a navigation: to the same kind of page in the new scope where there is one.
 */
export function switchTarget(input: SwitchInput): string {
	const org = input.org;
	// A workspace belongs to one organisation: changing the organisation drops it.
	const ws = org && input.was === org ? input.ws : '';
	// Insight pages that serve every scope stay open; a record (a call, a trace, an end user)
	// gives way to its list, since it belongs to one scope only.
	if (
		input.from.startsWith('/insight/') &&
		!INSTALLATION_INSIGHT.some((prefix) => isBelow(input.from, prefix))
	) {
		const page = input.from.split('/')[2] || 'requests';
		const list = page === 'traces' ? 'requests' : page;
		return `/insight/${encodeURIComponent(list)}${scopeQuery({ org: org || null, ws: ws || null })}`;
	}
	if (!org) return '/';
	// The same page in the new scope where there is one, its overview otherwise.
	if (!ws) {
		const page = input.from.match(/^\/orgs\/[^/]+\/([^/]+)$/)?.[1] ?? '';
		return organisationPath(org, ORGANISATION_PAGES.has(page) ? page : '');
	}
	const page = input.from.match(/^\/orgs\/[^/]+\/workspaces\/[^/]+\/([^/]+)/)?.[1] ?? '';
	return workspacePath(org, ws, WORKSPACE_PAGES.has(page) ? page : '');
}

/** An organisation or a workspace as the switcher offers it. */
export interface ScopeChoice {
	id: string;
	/** The slug. */
	name: string;
	/** The display name. */
	label: string;
	status?: string;
}

function fold(text: string): string {
	return text
		.normalize('NFKD')
		.replace(/[\u0300-\u036f]/g, '')
		.toLowerCase();
}

/**
 * The choices whose display name or slug contains every word of the query, ignoring case
 * and accents; those whose display name starts with it first.
 */
export function filterChoices<T extends ScopeChoice>(choices: T[], query: string): T[] {
	const words = fold(query).split(/\s+/).filter(Boolean);
	if (words.length === 0) return choices;
	const hits = choices.filter((c) => {
		const text = `${fold(c.label)} ${fold(c.name)}`;
		return words.every((w) => text.includes(w));
	});
	const starts = (c: T) => (fold(c.label).startsWith(words[0]) ? 0 : 1);
	return hits
		.map((c, i) => ({ c, i }))
		.sort((a, b) => starts(a.c) - starts(b.c) || a.i - b.i)
		.map((x) => x.c);
}

export interface Crumb {
	label: string;
	href: string;
}

/** One step of the scope bar: the installation, an organisation or a workspace. */
export interface TrailSegment extends Crumb {
	level: Level;
}

/**
 * Where the page shown lies, from the top down: `root`, then the organisation and the
 * workspace of the scope. Each step links to its own home page; a name that is not known
 * gives way to the id.
 */
export function scopeTrail(
	root: string,
	scope: Scope,
	orgs: readonly { id: string; label: string }[],
	workspaces: readonly { id: string; label: string }[]
): TrailSegment[] {
	const trail: TrailSegment[] = [{ level: 'top', label: root, href: '/' }];
	if (!scope.org) return trail;
	trail.push({
		level: 'organisation',
		label: orgs.find((o) => o.id === scope.org)?.label ?? scope.org,
		href: organisationPath(scope.org)
	});
	if (scope.ws) {
		trail.push({
			level: 'workspace',
			label: workspaces.find((w) => w.id === scope.ws)?.label ?? scope.ws,
			href: workspacePath(scope.org, scope.ws)
		});
	}
	return trail;
}

/** The address of another page of a cursor-paged list: the same filters, another cursor. */
export function pageLink(url: { pathname: string; search: string }, cursor: string | null): string {
	const query = new URLSearchParams(url.search);
	if (cursor) query.set('cursor', cursor);
	else query.delete('cursor');
	const text = query.toString();
	return url.pathname + (text ? `?${text}` : '');
}

/** The model page with the form for a draft opened and filled from what a provider lists. */
export function draftModelPath(
	provider: { id: string; name: string },
	model: {
		upstream_id: string;
		display_name?: string | null;
		context_window?: number | null;
		max_output_tokens?: number | null;
	}
): string {
	const query = new URLSearchParams({
		draft: '1',
		provider_id: provider.id,
		upstream_id: model.upstream_id,
		name: `${provider.name}/${model.upstream_id}`
	});
	if (model.display_name) query.set('display_name', model.display_name);
	if (model.context_window) query.set('context_window', String(model.context_window));
	if (model.max_output_tokens) query.set('max_output_tokens', String(model.max_output_tokens));
	return `/platform/models?${query}`;
}

/** The address of another page of a list that is paged by offset: the same filters. */
export function offsetLink(url: { pathname: string; search: string }, offset: number): string {
	const query = new URLSearchParams(url.search);
	if (offset > 0) query.set('offset', String(offset));
	else query.delete('offset');
	const text = query.toString();
	return url.pathname + (text ? `?${text}` : '');
}
