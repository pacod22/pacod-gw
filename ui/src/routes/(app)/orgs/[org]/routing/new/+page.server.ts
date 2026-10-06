import { redirect } from '@sveltejs/kit';
import { canWrite } from '#lib/access.ts';
import { organisationPath } from '#lib/nav.ts';
import { failed, type KitEvent } from '#lib/server/resource.ts';
import { schemeFromForm } from '#lib/server/routing.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const principal = event.locals.principal;
	return {
		writable: principal !== null && canWrite(principal, 'organisation', { org: event.params.org })
	};
};

export const actions = {
	save: async (event: KitEvent) => {
		const org = event.params.org ?? '';
		const read = schemeFromForm(await event.request.formData());
		if ('failure' in read) return read.failure;
		const result = await event.locals.api.POST('/orgs/{org}/routing-schemes', {
			params: { path: { org } },
			body: read.body as never
		});
		if (!result.response.ok || !result.data) return failed(event, 'save', result);
		redirect(303, `${organisationPath(org, 'routing')}/${result.data.id}?created=1`);
	}
};
