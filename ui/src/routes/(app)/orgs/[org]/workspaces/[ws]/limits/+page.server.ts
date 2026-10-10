import type { Schemas } from '#lib/api/types.ts';
import { readForm } from '#lib/schema/form.ts';
import { grantedModels, keyOptions, modelOptions, templateOptions } from '#lib/server/options.ts';
import {
	done,
	failed,
	formText,
	inputFields,
	problemOf,
	resource,
	type KitEvent
} from '#lib/server/resource.ts';

const limits = resource({
	path: '/orgs/{org}/workspaces/{ws}/limits',
	noun: 'limit',
	fields: async (event) => {
		const org = event.params.org ?? '';
		const ws = event.params.ws ?? '';
		const [models, templates, keys] = await Promise.all([
			grantedModels(event, org, ws),
			templateOptions(event, org, ws),
			keyOptions(event, org, ws)
		]);
		return {
			'filter.models': { label: 'Only these models', options: modelOptions(models, 'name') },
			'filter.templates': { label: 'Only these templates', options: templates },
			'filter.keys': {
				label: 'Only these API keys',
				help: 'A limit for one key, or for some keys. None chosen: every key of the workspace.',
				options: keys
			},
			'filter.tags': { label: 'Only calls with these tags' }
		};
	},
	// The live state: what is used of each limit, for the workspace or for one key or end user.
	extra: async (event) => {
		const end_user = event.url.searchParams.get('end_user') ?? '';
		const key_id = event.url.searchParams.get('key_id') ?? '';
		const path = { org: event.params.org ?? '', ws: event.params.ws ?? '' };
		const [result, keys] = await Promise.all([
			event.locals.api.GET('/orgs/{org}/workspaces/{ws}/limit-state', {
				params: {
					path,
					query: { ...(end_user ? { end_user } : {}), ...(key_id ? { key_id } : {}) }
				}
			}),
			// The keys by name, to choose one instead of typing its id.
			event.locals.api.GET('/orgs/{org}/workspaces/{ws}/keys', {
				params: { path, query: { limit: 500 } }
			})
		]);
		const state: Schemas['LimitState'][] = result.data ?? [];
		return {
			state,
			stateProblem: result.response.ok ? null : problemOf(event, result),
			stateFor: { end_user, key_id },
			keys: (keys.data?.items ?? []).map((k) => ({ id: k.id, name: k.name, status: k.status })),
			resetFields: inputFields('/orgs/{org}/workspaces/{ws}/limits/{id}/reset')
		};
	}
});

export const load = limits.load;

export const actions = {
	...limits.actions,

	/** Clears the counters of a limit, for the workspace or for one key or end user. */
	reset: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const fields = inputFields('/orgs/{org}/workspaces/{ws}/limits/{id}/reset');
		const subject = readForm(fields, form).subject;
		const result = await event.locals.api.POST('/orgs/{org}/workspaces/{ws}/limits/{id}/reset', {
			params: { path: { org: event.params.org ?? '', ws: event.params.ws ?? '', id } },
			body: { subject: typeof subject === 'string' ? subject : '' }
		});
		if (!result.response.ok) return failed(event, 'reset', result, { id, fields });
		return done('reset', `The counters of limit “${formText(form, 'name')}” were reset.`, { id });
	}
};
