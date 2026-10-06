import { error } from '@sveltejs/kit';
import { canAdminOrg, canReadOrg } from '#lib/access.ts';
import { problemOf } from '#lib/server/resource.ts';
import { countOf } from '#lib/server/settings.ts';
import type { PageServerLoad } from './$types';

/** The organisation at a glance: its workspaces, who works in it, and what it was given. */
export const load: PageServerLoad = async (event) => {
	const org = event.params.org;
	const principal = event.locals.principal;
	const reader = principal !== null && canReadOrg(principal, org);
	const admin = principal !== null && canAdminOrg(principal, org);
	const api = event.locals.api;

	const [item, workspaces, members, invitations, grants, limits, schemes] = await Promise.all([
		api.GET('/orgs/{org}', { params: { path: { org } } }),
		api.GET('/orgs/{org}/workspaces', { params: { path: { org }, query: { limit: 500 } } }),
		reader ? api.GET('/orgs/{org}/members', { params: { path: { org } } }) : null,
		admin ? api.GET('/orgs/{org}/invitations', { params: { path: { org } } }) : null,
		reader ? countOf(event, '/orgs/{org}/grants', { org }) : null,
		reader ? countOf(event, '/orgs/{org}/limits', { org }) : null,
		reader ? countOf(event, '/orgs/{org}/routing-schemes', { org }) : null
	]);
	if (!item.response.ok) {
		const problem = problemOf(event, item);
		error(problem.status === 404 ? 404 : problem.status || 502, problem.message);
	}

	const people = members?.data?.items ?? [];
	return {
		organisation: item.data!,
		workspaces: workspaces.data?.items ?? [],
		reader,
		admin,
		memberCount: members?.data ? people.length : null,
		admins: people
			.filter((m) => m.roles.some((r) => r.role === 'org_admin' && r.scope.type === 'organisation'))
			.map((m) => m.name),
		pending: invitations?.data ? invitations.data.items.filter((i) => !i.expired).length : null,
		grants: grants?.count ?? null,
		limits: limits?.count ?? null,
		schemes: schemes?.count ?? null
	};
};
