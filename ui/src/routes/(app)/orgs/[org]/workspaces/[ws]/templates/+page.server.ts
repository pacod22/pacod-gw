import { fail, redirect } from '@sveltejs/kit';
import { workspacePath } from '#lib/nav.ts';
import { failed, formText, resource, type KitEvent } from '#lib/server/resource.ts';

/** How far back "last used" looks. */
const LAST_USED_DAYS = 90;

/** When each template of the workspace was last called. Empty when analytics cannot answer. */
async function lastUsed(event: KitEvent): Promise<Record<string, string>> {
	const from = new Date(Date.now() - LAST_USED_DAYS * 86_400_000).toISOString();
	const result = await event.locals.api.GET('/analytics/last-used', {
		params: {
			query: {
				by: 'template',
				from,
				organisation_id: event.params.org ?? '',
				workspace_id: event.params.ws ?? ''
			} as never
		}
	});
	const rows = (
		(result.data as { rows?: { group: string; last_used: string }[] } | undefined)?.rows ?? []
	).filter((r) => r.group);
	return Object.fromEntries(rows.map((r) => [r.group, r.last_used]));
}

// The list and the deletion come from the kit; a template is created and edited on pages
// of its own, because its form is not the generic one.
const templates = resource({
	path: '/orgs/{org}/workspaces/{ws}/templates',
	noun: 'template',
	extra: async (event) => ({ lastUsed: await lastUsed(event), lastUsedDays: LAST_USED_DAYS })
});

export const load = templates.load;
export const actions = {
	delete: templates.actions.delete,

	/** Creates a template from a file exported elsewhere (`…/templates/{id}/export`). */
	import: async (event: KitEvent) => {
		const form = await event.request.formData();
		const org = event.params.org ?? '';
		const ws = event.params.ws ?? '';
		let file: { name?: unknown; description?: unknown; spec?: unknown };
		try {
			const upload = form.get('file');
			const text =
				upload instanceof File && upload.size > 0 ? await upload.text() : formText(form, 'text');
			file = JSON.parse(text);
		} catch {
			return fail(400, {
				action: 'import',
				ok: false,
				message: 'The file is not a template file: it is not JSON.'
			});
		}
		const name = formText(form, 'name') || (typeof file.name === 'string' ? file.name : '');
		const result = await event.locals.api.POST('/orgs/{org}/workspaces/{ws}/templates', {
			params: { path: { org, ws } },
			body: {
				name,
				description: typeof file.description === 'string' ? file.description : '',
				spec: file.spec as never,
				comment: 'imported'
			} as never
		});
		if (!result.response.ok || !result.data) return failed(event, 'import', result);
		redirect(303, `${workspacePath(org, ws, 'templates')}/${result.data.id}?created=1`);
	}
};
