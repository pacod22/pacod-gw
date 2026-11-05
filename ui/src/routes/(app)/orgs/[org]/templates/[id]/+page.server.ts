import { error } from '@sveltejs/kit';
import { done, failed, formText, problemOf, type KitEvent } from '#lib/server/resource.ts';
import { declaredTagKeys, editorContext, plainTemplateFields } from '#lib/server/templates.ts';
import { readTemplateForm } from '#lib/templates.ts';
import type { PageServerLoad } from './$types';

// An organisation's template has no test call, no routing explanation and no copy: each of
// those needs a workspace, where the template's own page offers them.

function path(event: KitEvent) {
	return { org: event.params.org ?? '', id: event.params.id ?? '' };
}

export const load: PageServerLoad = async (event) => {
	const [template, check, context, versions] = await Promise.all([
		event.locals.api.GET('/orgs/{org}/templates/{id}', { params: { path: path(event) } }),
		event.locals.api.GET('/orgs/{org}/templates/{id}/check', { params: { path: path(event) } }),
		editorContext(event),
		// Every version, also those the template itself no longer carries.
		event.locals.api.GET('/orgs/{org}/templates/{id}/versions', {
			params: { path: path(event) }
		})
	]);
	if (template.response.status === 404) error(404, 'This template does not exist.');
	return {
		template: template.data ?? null,
		problem: template.response.ok ? null : problemOf(event, template),
		check: check.data ?? null,
		allVersions: versions.data ?? null,
		context,
		created: event.url.searchParams.has('created')
	};
};

export const actions = {
	/** Saves the form as a new version. */
	save: async (event: KitEvent) => {
		const form = await event.request.formData();
		const fields = plainTemplateFields('organisation');
		const body = readTemplateForm(form, fields, await declaredTagKeys(event));
		const version = Number(formText(form, 'version'));
		if (formText(form, 'version') !== '' && Number.isInteger(version)) body.version = version;
		const result = await event.locals.api.PUT('/orgs/{org}/templates/{id}', {
			params: { path: path(event) },
			body: body as never
		});
		if (!result.response.ok || !result.data) return failed(event, 'save', result, { fields });
		return done(
			'save',
			`Template “${result.data.name}” was saved as version ${result.data.current_version}.`
		);
	},

	/** Makes an earlier version the one calls use again. The versions after it stay. */
	rollback: async (event: KitEvent) => {
		const form = await event.request.formData();
		const version = Number(formText(form, 'target'));
		const result = await event.locals.api.POST('/orgs/{org}/templates/{id}/rollback', {
			params: { path: path(event) },
			body: { version }
		});
		if (!result.response.ok || !result.data) return failed(event, 'rollback', result);
		return done(
			'rollback',
			`Template “${result.data.name}” is back at version ${result.data.current_version}.`
		);
	}
};
