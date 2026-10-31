import { error, redirect } from '@sveltejs/kit';
import { accountUrl, invitationsUrl } from '#lib/identity.ts';
import { KEPT_SCOPE_COOKIE, PLATFORM_NAV_COOKIE, parseKeptScope } from '#lib/nav.ts';
import { identity } from '#lib/server/identity.ts';
import type { LayoutServerLoad } from './$types';

/** How many organisations and workspaces the switcher offers. */
const SWITCHER_LIMIT = 500;

export const load: LayoutServerLoad = async ({ locals, params, url, cookies }) => {
	const principal = locals.principal;
	if (!principal) {
		// The hook lets a request through without a principal only when the core did not answer.
		error(502, locals.apiProblem?.message ?? 'The management API of the gateway did not answer.');
	}
	// With pacod-id, anyone of its realm can sign in: someone without a role here yet sees a
	// page that says so, not an empty console.
	if (
		identity &&
		principal.kind === 'user' &&
		principal.roles.length === 0 &&
		!principal.platform_admin
	) {
		redirect(303, '/no-access');
	}

	// The selected organisation and workspace live in the URL: in the path of the pages below
	// `/orgs`, in the query of the pages that serve every scope. The pathname is not read here,
	// so that moving between pages does not load the lists again; the layout works out which
	// pages take their scope from the query.
	const org = params.org ?? url.searchParams.get('organisation_id');
	const ws = params.org ? (params.ws ?? null) : org ? url.searchParams.get('workspace_id') : null;
	// The scope the sidebar keeps for the pages without one of their own, as the browser last
	// wrote it. It is checked here: the organisation may be gone, or no longer the caller's.
	const kept = parseKeptScope(cookies.get(KEPT_SCOPE_COOKIE));
	const keptOrg = kept.org && kept.org !== org ? kept.org : null;

	const workspacesOf = (id: string) =>
		locals.api.GET('/orgs/{org}/workspaces', {
			params: { path: { org: id }, query: { limit: SWITCHER_LIMIT } }
		});
	const [orgs, workspaces, keptWorkspaces] = await Promise.all([
		locals.api.GET('/orgs', { params: { query: { limit: SWITCHER_LIMIT } } }),
		org ? workspacesOf(org) : null,
		keptOrg ? workspacesOf(keptOrg) : null
	]);

	const brief = (item: { id: string; name: string; display_name?: string; status?: string }) => ({
		id: item.id,
		name: item.name,
		label: item.display_name || item.name,
		status: item.status
	});
	// The kept organisation's workspaces are the URL's when both name the same organisation.
	const keptList = kept.org ? (keptOrg ? keptWorkspaces : workspaces) : null;
	const keptChoices = keptList?.response.ok ? (keptList.data?.items ?? []).map(brief) : null;

	return {
		principal,
		/** With pacod-id: where people manage their account and find their invitations. */
		identity: identity
			? { account: accountUrl(identity.issuer), invitations: invitationsUrl(identity.issuer) }
			: null,
		scope: { org: org || null, ws: ws || null },
		orgs: (orgs.data?.items ?? []).map(brief),
		workspaces: (workspaces?.data?.items ?? []).map(brief),
		/** The scope kept for the sidebar of pages without one; the layout keeps it current. */
		kept: keptChoices
			? { org: kept.org, ws: keptChoices.some((w) => w.id === kept.ws) ? kept.ws : null }
			: { org: null, ws: null },
		keptWorkspaces: keptChoices ?? [],
		platformOpen: cookies.get(PLATFORM_NAV_COOKIE) !== 'closed'
	};
};
