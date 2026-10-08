import { error } from '@sveltejs/kit';
import { canAdminOrg, canReadOrg } from '#lib/access.ts';
import { fillSettings, loadedMessage, planTemplate } from '#lib/org-template.ts';
import { done, failed, problemOf, type KitEvent, type Outcome } from '#lib/server/resource.ts';
import { createMissingSchemes } from '#lib/server/routing.ts';
import { countOf } from '#lib/server/settings.ts';
import type { Actions, PageServerLoad } from './$types';

/** The `details.code` of an API error, such as `routing_scheme_unknown`. */
function errorCode(result: { error?: unknown }): string {
	const body = result.error as { error?: { details?: { code?: string } } } | undefined;
	return body?.error?.details?.code ?? '';
}

/** The organisation template and the names of the organisation's routing schemes. */
async function templateAndSchemes(event: KitEvent, org: string) {
	const api = event.locals.api;
	const [template, schemes] = await Promise.all([
		api.GET('/org-config-template'),
		api.GET('/orgs/{org}/routing-schemes', { params: { path: { org }, query: { limit: 500 } } })
	]);
	return { template, schemes, taken: (schemes.data?.items ?? []).map((s) => s.name) };
}

/** The organisation at a glance: its workspaces, who works in it, and what it was given. */
export const load: PageServerLoad = async (event) => {
	const org = event.params.org;
	const principal = event.locals.principal;
	const reader = principal !== null && canReadOrg(principal, org);
	const admin = principal !== null && canAdminOrg(principal, org);
	const api = event.locals.api;

	const [item, workspaces, members, invitations, grants, limits, schemes, loadable] =
		await Promise.all([
			api.GET('/orgs/{org}', { params: { path: { org } } }),
			api.GET('/orgs/{org}/workspaces', { params: { path: { org }, query: { limit: 500 } } }),
			reader ? api.GET('/orgs/{org}/members', { params: { path: { org } } }) : null,
			admin ? api.GET('/orgs/{org}/invitations', { params: { path: { org } } }) : null,
			reader ? countOf(event, '/orgs/{org}/grants', { org }) : null,
			reader ? countOf(event, '/orgs/{org}/limits', { org }) : null,
			reader ? countOf(event, '/orgs/{org}/routing-schemes', { org }) : null,
			admin ? templateAndSchemes(event, org) : null
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
		schemes: schemes?.count ?? null,
		// What loading the template would change, for the admins who may load it.
		template:
			loadable?.template.data && loadable.schemes.data
				? planTemplate(loadable.template.data, item.data!.settings, loadable.taken)
				: null
	};
};

export const actions: Actions = {
	/**
	 * Loads the organisation template: the routing schemes the organisation lacks first, then
	 * the settings it has no value for, which may name those schemes. What it has is kept.
	 */
	loadTemplate: async (event): Promise<Outcome> => {
		const org = event.params.org;
		const api = event.locals.api;
		const [{ template, schemes, taken }, item] = await Promise.all([
			templateAndSchemes(event, org),
			api.GET('/orgs/{org}', { params: { path: { org } } })
		]);
		for (const result of [template, schemes, item]) {
			if (!result.response.ok) return failed(event, 'loadTemplate', result);
		}
		const plan = planTemplate(template.data!, item.data!.settings, taken);

		const made = await createMissingSchemes(
			event,
			'loadTemplate',
			org,
			template.data!.routing_schemes,
			plan.schemes
		);
		if ('failure' in made) return made.failure;
		const created = made.created;

		const set = plan.settings.filter((s) => s.apply);
		if (set.length > 0) {
			const body = { ...item.data!, settings: fillSettings(template.data!, item.data!.settings) };
			// With several admin instances, the one answering may not have applied the schemes
			// just created; each catches up within two seconds.
			for (let attempt = 1; ; attempt++) {
				const result = await api.PUT('/orgs/{org}', { params: { path: { org } }, body });
				if (result.response.ok) break;
				if (errorCode(result) !== 'routing_scheme_unknown' || attempt === 6) {
					return failed(event, 'loadTemplate', result);
				}
				await new Promise((resolve) => setTimeout(resolve, 500));
			}
		}
		return done(
			'loadTemplate',
			loadedMessage(
				created,
				set.map((s) => s.label)
			),
			{ tone: plan.changes === 0 ? 'warning' : 'success' }
		);
	}
};
