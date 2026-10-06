import { error } from '@sveltejs/kit';
import { readForm } from '#lib/schema/form.ts';
import { workspaceOptions } from '#lib/server/options.ts';
import {
	done,
	failed,
	formText,
	inputFields,
	problemOf,
	type KitEvent
} from '#lib/server/resource.ts';
import { declaredTagKeys, editorContext, plainTemplateFields } from '#lib/server/templates.ts';
import { readTemplateForm } from '#lib/templates.ts';
import type { PageServerLoad } from './$types';

const CLONE = '/orgs/{org}/workspaces/{ws}/templates/{id}/clone';

function path(event: KitEvent) {
	return { org: event.params.org ?? '', ws: event.params.ws ?? '', id: event.params.id ?? '' };
}

async function cloneFields(event: KitEvent) {
	const workspaces = (await workspaceOptions(event)).filter((w) => w.value !== event.params.ws);
	return inputFields(CLONE, 'post', {
		workspace_id: {
			label: 'Workspace to copy it into',
			...(workspaces.length > 0 ? { options: workspaces } : {})
		},
		name: { label: 'Name of the copy', help: 'Leave it empty to keep the name.' }
	});
}

export const load: PageServerLoad = async (event) => {
	const [template, check, context] = await Promise.all([
		event.locals.api.GET('/orgs/{org}/workspaces/{ws}/templates/{id}', {
			params: { path: path(event) }
		}),
		event.locals.api.GET('/orgs/{org}/workspaces/{ws}/templates/{id}/check', {
			params: { path: path(event) }
		}),
		editorContext(event)
	]);
	if (template.response.status === 404) error(404, 'This template does not exist.');
	return {
		template: template.data ?? null,
		problem: template.response.ok ? null : problemOf(event, template),
		check: check.data ?? null,
		context,
		cloneFields: context.writable ? await cloneFields(event) : [],
		created: event.url.searchParams.has('created')
	};
};

export const actions = {
	/** Saves the form as a new version. */
	save: async (event: KitEvent) => {
		const form = await event.request.formData();
		const fields = plainTemplateFields();
		const body = readTemplateForm(form, fields, await declaredTagKeys(event));
		const version = Number(formText(form, 'version'));
		if (formText(form, 'version') !== '' && Number.isInteger(version)) body.version = version;
		const result = await event.locals.api.PUT('/orgs/{org}/workspaces/{ws}/templates/{id}', {
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
		const result = await event.locals.api.POST(
			'/orgs/{org}/workspaces/{ws}/templates/{id}/rollback',
			{
				params: { path: path(event) },
				body: { version }
			}
		);
		if (!result.response.ok || !result.data) return failed(event, 'rollback', result);
		return done(
			'rollback',
			`Template “${result.data.name}” is back at version ${result.data.current_version}.`
		);
	},

	/** How routing would treat a call: the rule that matches and the models in order. */
	route: async (event: KitEvent) => {
		const form = await event.request.formData();
		const opt = (name: string) => formText(form, name).trim() || null;
		const tags: Record<string, string> = {};
		for (const pair of formText(form, 'tags').split(',')) {
			const at = pair.indexOf('=');
			if (at > 0) tags[pair.slice(0, at).trim()] = pair.slice(at + 1).trim();
		}
		const result = await event.locals.api.POST('/orgs/{org}/workspaces/{ws}/templates/{id}/route', {
			params: { path: path(event) },
			body: {
				complexity: opt('complexity'),
				agent_id: opt('agent_id'),
				task: opt('task'),
				user_id: opt('user_id'),
				session_id: opt('session_id'),
				tags
			} as never
		});
		if (!result.response.ok || !result.data) return failed(event, 'route', result);
		return done('route', '', { data: result.data });
	},

	/** Copies the template into another workspace. */
	clone: async (event: KitEvent) => {
		const form = await event.request.formData();
		const fields = await cloneFields(event);
		const input = readForm(fields, form);
		const result = await event.locals.api.POST(CLONE, {
			params: { path: path(event) },
			body: {
				workspace_id: typeof input.workspace_id === 'string' ? input.workspace_id : '',
				name: typeof input.name === 'string' && input.name ? input.name : null
			}
		});
		if (!result.response.ok || !result.data) return failed(event, 'clone', result, { fields });
		return done('clone', `The template was copied as “${result.data.name}”.`);
	}
};
