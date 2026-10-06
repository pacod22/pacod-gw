import { error } from '@sveltejs/kit';
import { canWrite } from '#lib/access.ts';
import { done, failed, formText, problemOf, type KitEvent } from '#lib/server/resource.ts';
import { schemeFromForm } from '#lib/server/routing.ts';
import type { PageServerLoad } from './$types';

function path(event: KitEvent) {
	return { org: event.params.org ?? '', id: event.params.id ?? '' };
}

export const load: PageServerLoad = async (event) => {
	const scheme = await event.locals.api.GET('/orgs/{org}/routing-schemes/{id}', {
		params: { path: path(event) }
	});
	if (scheme.response.status === 404) error(404, 'This routing scheme does not exist.');
	const principal = event.locals.principal;
	return {
		scheme: scheme.data ?? null,
		problem: scheme.response.ok ? null : problemOf(event, scheme),
		writable: principal !== null && canWrite(principal, 'organisation', { org: event.params.org }),
		created: event.url.searchParams.has('created')
	};
};

export const actions = {
	save: async (event: KitEvent) => {
		const form = await event.request.formData();
		const read = schemeFromForm(form);
		if ('failure' in read) return read.failure;
		const body = read.body as Record<string, unknown>;
		const version = Number(formText(form, 'version'));
		if (Number.isInteger(version) && version > 0) body.version = version;
		const result = await event.locals.api.PUT('/orgs/{org}/routing-schemes/{id}', {
			params: { path: path(event) },
			body: body as never
		});
		if (!result.response.ok || !result.data) return failed(event, 'save', result);
		return done('save', `Routing scheme “${result.data.name}” was saved. Calls use it at once.`);
	}
};
