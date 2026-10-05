import { error, redirect } from '@sveltejs/kit';
import { canWrite } from '#lib/access.ts';
import { workspacePath } from '#lib/nav.ts';
import { failed, problemOf, type KitEvent } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

function path(event: KitEvent) {
	return { org: event.params.org ?? '', ws: event.params.ws ?? '', id: event.params.id ?? '' };
}

export const load: PageServerLoad = async (event) => {
	const number = Number(event.params.n);
	if (!Number.isInteger(number) || number < 1) error(404, 'A version is named by its number.');
	const [template, version] = await Promise.all([
		event.locals.api.GET('/orgs/{org}/workspaces/{ws}/templates/{id}', {
			params: { path: path(event) }
		}),
		event.locals.api.GET('/orgs/{org}/workspaces/{ws}/templates/{id}/versions/{version}', {
			params: { path: { ...path(event), version: String(number) } }
		})
	]);
	if (template.response.status === 404) error(404, 'This template does not exist.');
	if (version.response.status === 404) error(404, `This template has no version ${number}.`);
	const principal = event.locals.principal;
	return {
		name: template.data?.name ?? '',
		currentVersion: template.data?.current_version ?? 0,
		version: version.data ?? null,
		problem: version.response.ok ? null : problemOf(event, version),
		writable: principal !== null && canWrite(principal, 'workspace', path(event))
	};
};

export const actions = {
	rollback: async (event: KitEvent) => {
		const result = await event.locals.api.POST(
			'/orgs/{org}/workspaces/{ws}/templates/{id}/rollback',
			{
				params: { path: path(event) },
				body: { version: Number(event.params.n) }
			}
		);
		if (!result.response.ok) return failed(event, 'rollback', result);
		const { org, ws, id } = path(event);
		redirect(303, `${workspacePath(org, ws, 'templates')}/${id}`);
	}
};
