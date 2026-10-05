import { redirect } from '@sveltejs/kit';
import { workspacePath } from '#lib/nav.ts';
import { failed, type KitEvent } from '#lib/server/resource.ts';
import { declaredTagKeys, editorContext, plainTemplateFields } from '#lib/server/templates.ts';
import { readTemplateForm } from '#lib/templates.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => ({ context: await editorContext(event) });

export const actions = {
	save: async (event: KitEvent) => {
		const org = event.params.org ?? '';
		const ws = event.params.ws ?? '';
		const form = await event.request.formData();
		const fields = plainTemplateFields();
		const result = await event.locals.api.POST('/orgs/{org}/workspaces/{ws}/templates', {
			params: { path: { org, ws } },
			body: readTemplateForm(form, fields, await declaredTagKeys(event)) as never
		});
		if (!result.response.ok || !result.data) return failed(event, 'save', result, { fields });
		redirect(303, `${workspacePath(org, ws, 'templates')}/${result.data.id}?created=1`);
	}
};
