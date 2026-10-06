/**
 * The navigation of the console, grouped as in concept section 15.2, reduced to what the
 * principal's roles allow. Organisation and workspace pages carry the selection in the path;
 * Insight pages carry it in the query, so one page serves every scope.
 */
import { canReadAuditLog, canReadAuditRecords, hasPlatformRole, type Scope } from './access';
import type { Principal } from './api/types';

export interface NavItem {
	label: string;
	href: string;
	/** Path prefix that marks the item as current; defaults to the path of `href`. */
	match?: string;
	/** Current only on exactly this path, not on the pages below it. */
	exact?: boolean;
}

export interface NavGroup {
	label: string;
	items: NavItem[];
	/** Shown in place of the items when the group needs a selection first. */
	hint?: string;
}

/** The query that carries the selected scope to pages that are not below `/orgs`. */
export function scopeQuery(scope: Scope): string {
	const query = new URLSearchParams();
	if (scope.org) query.set('organisation_id', scope.org);
	if (scope.org && scope.ws) query.set('workspace_id', scope.ws);
	const text = query.toString();
	return text ? `?${text}` : '';
}

export function workspacePath(org: string, ws: string, page = 'keys'): string {
	return `/orgs/${encodeURIComponent(org)}/workspaces/${encodeURIComponent(ws)}/${page}`;
}

export function organisationPath(org: string, page = 'workspaces'): string {
	return `/orgs/${encodeURIComponent(org)}/${page}`;
}

export function buildNav(principal: Principal, scope: Scope): NavGroup[] {
	const groups: NavGroup[] = [{ label: 'Home', items: [{ label: 'Dashboard', href: '/' }] }];

	if (hasPlatformRole(principal)) {
		const items: NavItem[] = [
			{ label: 'Providers', href: '/platform/providers' },
			{ label: 'Models', href: '/platform/models' },
			{ label: 'Prices', href: '/platform/prices' },
			...(principal.platform_admin
				? [{ label: 'Price tracker', href: '/platform/price-tracker' }]
				: []),
			{ label: 'Organisations', href: '/platform/organisations' }
		];
		if (principal.platform_admin) items.push({ label: 'Access', href: '/platform/access' });
		if (principal.platform_admin || canReadAuditLog(principal)) {
			items.push({
				label: 'System',
				href: principal.platform_admin ? '/platform/system' : '/platform/system/audit-log',
				match: '/platform/system'
			});
		}
		items.push({ label: 'Settings', href: '/platform/settings' });
		groups.push({ label: 'Platform', items });
	}

	groups.push(
		scope.org
			? {
					label: 'Organisation',
					items: [
						{ label: 'Workspaces', href: organisationPath(scope.org), exact: true },
						{ label: 'Model grants', href: organisationPath(scope.org, 'grants') },
						{ label: 'Limits', href: organisationPath(scope.org, 'limits') }
					]
				}
			: { label: 'Organisation', items: [], hint: 'Choose an organisation to see its pages.' }
	);

	groups.push(
		scope.org && scope.ws
			? {
					label: 'Workspace',
					items: [
						{ label: 'Model grants', href: workspacePath(scope.org, scope.ws, 'grants') },
						{ label: 'Templates', href: workspacePath(scope.org, scope.ws, 'templates') },
						{ label: 'API keys', href: workspacePath(scope.org, scope.ws, 'keys') },
						{ label: 'Limits', href: workspacePath(scope.org, scope.ws, 'limits') },
						{ label: 'Playground', href: workspacePath(scope.org, scope.ws, 'playground') },
						{ label: 'Settings', href: workspacePath(scope.org, scope.ws, 'settings') }
					]
				}
			: { label: 'Workspace', items: [], hint: 'Choose a workspace to see its pages.' }
	);

	// Insight pages serve every scope: the selection travels in the query.
	const query = scopeQuery(scope);
	const insight = (label: string, page: string): NavItem => ({
		label,
		href: `/insight/${page}${query}`,
		match: `/insight/${page}`
	});
	groups.push({
		label: 'Insight',
		items: [
			insight('Analytics', 'analytics'),
			insight('Usage and costs', 'usage'),
			insight('Breakdown', 'breakdown'),
			insight('End users', 'end-users'),
			insight('Errors', 'errors'),
			insight('Requests', 'requests'),
			// Audit records hold prompts and answers: not for viewers.
			...(canReadAuditRecords(principal) ? [insight('Audit records', 'audit-records')] : []),
			insight('Monitoring', 'monitoring'),
			// Prometheus counts every organisation alike.
			...(hasPlatformRole(principal) ? [insight('Metrics', 'metrics')] : [])
		]
	});

	return groups;
}

/** Whether a navigation item points at the page that is shown, or at one above it. */
export function isCurrent(item: NavItem, pathname: string): boolean {
	const target = item.match ?? item.href.split('?')[0];
	if (target === '/' || item.exact) return pathname === target;
	return pathname === target || pathname.startsWith(`${target}/`);
}

/** The pages below a workspace that stay open while the switcher changes the workspace. */
const WORKSPACE_PAGES = new Set([
	'grants',
	'templates',
	'keys',
	'limits',
	'playground',
	'settings'
]);
/** The pages below an organisation that stay open while the switcher changes the organisation. */
const ORGANISATION_PAGES = new Set(['workspaces', 'grants', 'limits']);

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
	if (input.from.startsWith('/insight/')) {
		const page = input.from.split('/')[2] || 'requests';
		return `/insight/${encodeURIComponent(page)}${scopeQuery({ org: org || null, ws: ws || null })}`;
	}
	if (!org) return '/';
	if (!ws) {
		const page = input.from.match(/^\/orgs\/[^/]+\/([^/]+)$/)?.[1] ?? 'workspaces';
		return organisationPath(org, ORGANISATION_PAGES.has(page) ? page : 'workspaces');
	}
	const page = input.from.match(/^\/orgs\/[^/]+\/workspaces\/[^/]+\/([^/]+)/)?.[1] ?? 'keys';
	return workspacePath(org, ws, WORKSPACE_PAGES.has(page) ? page : 'keys');
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
