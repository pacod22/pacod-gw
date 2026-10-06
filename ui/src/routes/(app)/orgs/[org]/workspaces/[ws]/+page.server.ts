import { error } from '@sveltejs/kit';
import { canAdminWorkspace } from '#lib/access.ts';
import { problemOf } from '#lib/server/resource.ts';
import { countOf } from '#lib/server/settings.ts';
import type { PageServerLoad } from './$types';

/** The workspace at a glance: its keys, templates, model access, limits and members. */
export const load: PageServerLoad = async (event) => {
	const { org, ws } = event.params;
	const path = { org, ws };
	const api = event.locals.api;
	const principal = event.locals.principal;

	const [item, keys, templates, grants, orgGrants, limits, members] = await Promise.all([
		api.GET('/orgs/{org}/workspaces/{ws}', { params: { path } }),
		api.GET('/orgs/{org}/workspaces/{ws}/keys', { params: { path, query: { limit: 500 } } }),
		countOf(event, '/orgs/{org}/workspaces/{ws}/templates', path),
		countOf(event, '/orgs/{org}/workspaces/{ws}/grants', path),
		countOf(event, '/orgs/{org}/grants', { org }),
		countOf(event, '/orgs/{org}/workspaces/{ws}/limits', path),
		api.GET('/orgs/{org}/workspaces/{ws}/members', { params: { path } })
	]);
	if (!item.response.ok) {
		const problem = problemOf(event, item);
		error(problem.status === 404 ? 404 : problem.status || 502, problem.message);
	}

	const keyItems = keys.data?.items ?? [];
	return {
		workspace: item.data!,
		admin: principal !== null && canAdminWorkspace(principal, ws, org),
		keys: keys.data
			? {
					active: keyItems.filter((k) => k.status === 'active').length,
					revoked: keyItems.filter((k) => k.status !== 'active').length
				}
			: null,
		templates: templates?.count ?? null,
		grants: grants?.count ?? null,
		orgGrants: orgGrants?.count ?? null,
		limits: limits?.count ?? null,
		members: members.data
			? members.data.items.filter((m) => m.roles.some((r) => !r.inherited)).length
			: null
	};
};
