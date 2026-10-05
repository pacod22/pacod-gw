import { error } from '@sveltejs/kit';
import type { LayoutServerLoad } from './$types';

/** How many organisations and workspaces the switcher offers. */
const SWITCHER_LIMIT = 500;

export const load: LayoutServerLoad = async ({ locals, params, url }) => {
	const principal = locals.principal;
	if (!principal) {
		// The hook lets a request through without a principal only when the core did not answer.
		error(502, locals.apiProblem?.message ?? 'The management API of the gateway did not answer.');
	}

	// The selected organisation and workspace live in the URL: in the path of the pages below
	// `/orgs`, in the query of the pages that serve every scope.
	const org = params.org ?? url.searchParams.get('organisation_id');
	const ws = org ? (params.ws ?? url.searchParams.get('workspace_id')) : null;

	const [orgs, workspaces] = await Promise.all([
		locals.api.GET('/orgs', { params: { query: { limit: SWITCHER_LIMIT } } }),
		org
			? locals.api.GET('/orgs/{org}/workspaces', {
					params: { path: { org }, query: { limit: SWITCHER_LIMIT } }
				})
			: null
	]);

	const brief = (item: { id: string; name: string; display_name?: string }) => ({
		id: item.id,
		name: item.name,
		label: item.display_name || item.name
	});

	return {
		principal,
		scope: { org: org || null, ws: ws || null },
		orgs: (orgs.data?.items ?? []).map(brief),
		workspaces: (workspaces?.data?.items ?? []).map(brief)
	};
};
