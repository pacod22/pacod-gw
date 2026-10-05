import { error } from '@sveltejs/kit';
import { problemOf } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const path = { id: event.params.id };
	const [provider, discovery] = await Promise.all([
		event.locals.api.GET('/providers/{id}', { params: { path } }),
		event.locals.api.GET('/providers/{id}/discovery', { params: { path } })
	]);
	if (provider.response.status === 404) error(404, 'This provider does not exist.');
	return {
		provider: provider.data ? { id: provider.data.id, name: provider.data.name } : null,
		discovery: discovery.data ?? null,
		problem: discovery.response.ok ? null : problemOf(event, discovery),
		mayCreate: event.locals.principal?.platform_admin === true
	};
};
