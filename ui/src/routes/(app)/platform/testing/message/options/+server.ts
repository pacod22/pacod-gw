import { json } from '@sveltejs/kit';
import { requirePlatformAdmin } from '#lib/server/tester.ts';
import type { RequestHandler } from './$types';

export interface MessageOptions {
	workspaces: { id: string; name: string }[];
	templates: string[];
	models: { name: string; provider: string; family: string; granted: boolean }[];
	providers: { name: string; family: string }[];
	/** The workspace's own audit mode; null when it takes the organisation's or the platform's. */
	audit: string | null;
}

/**
 * What the composer offers for an organisation and workspace: asked when the selection changes,
 * so that the rest of the form stays as it is.
 */
export const GET: RequestHandler = async ({ locals, url }) => {
	requirePlatformAdmin(locals);
	const org = url.searchParams.get('org') ?? '';
	const ws = url.searchParams.get('ws') ?? '';
	const api = locals.api;
	const [workspaces, models, providers, grants, templates, workspace] = await Promise.all([
		org
			? api.GET('/orgs/{org}/workspaces', { params: { path: { org }, query: { limit: 500 } } })
			: null,
		api.GET('/models'),
		api.GET('/providers', { params: { query: { limit: 500 } } }),
		org
			? api.GET('/orgs/{org}/grants', { params: { path: { org }, query: { limit: 500 } } })
			: null,
		org && ws
			? api.GET('/orgs/{org}/workspaces/{ws}/templates', {
					params: { path: { org, ws }, query: { limit: 500 } }
				})
			: null,
		org && ws ? api.GET('/orgs/{org}/workspaces/{ws}', { params: { path: { org, ws } } }) : null
	]);
	const family = new Map((providers.data?.items ?? []).map((p) => [p.id, String(p.family)]));
	const granted = new Set((grants?.data?.items ?? []).map((g) => g.model_id));
	const settings = (workspace?.data?.settings ?? {}) as { audit?: { mode?: string | null } | null };
	const body: MessageOptions = {
		workspaces: (workspaces?.data?.items ?? []).map((w) => ({
			id: w.id,
			name: w.display_name || w.name
		})),
		templates: (templates?.data?.items ?? []).map((t) => t.name).sort(),
		models: (models.data?.items ?? [])
			.filter((m) => m.state === 'active' || m.state === 'deprecated')
			.map((m) => ({
				name: m.name,
				provider: m.provider ?? '',
				family: family.get(m.provider_id) ?? '',
				granted: granted.has(m.id)
			}))
			.sort((a, b) => Number(b.granted) - Number(a.granted) || a.name.localeCompare(b.name)),
		providers: (providers.data?.items ?? [])
			.map((p) => ({ name: p.name, family: String(p.family) }))
			.sort((a, b) => a.name.localeCompare(b.name)),
		audit: settings.audit?.mode ?? null
	};
	return json(body);
};
