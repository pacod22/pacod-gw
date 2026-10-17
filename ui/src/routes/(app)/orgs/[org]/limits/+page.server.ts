import { grantedModels, modelOptions } from '#lib/server/options.ts';
import { done, failed, formText, resource, type KitEvent } from '#lib/server/resource.ts';

const limits = resource({
	path: '/orgs/{org}/limits',
	noun: 'limit',
	// The cap of an organisation is set by the platform.
	writers: 'platform',
	fields: async (event) => ({
		'filter.models': {
			label: 'Only these models',
			options: modelOptions(await grantedModels(event, event.params.org ?? ''), 'name')
		},
		'filter.templates': { label: 'Only these templates' },
		'filter.tags': { label: 'Only calls with these tags' },
		'filter.intents': {
			label: 'Only calls with these intents',
			help: 'As an enforcing intent plugin found them. A call without one is not counted.'
		},
		// An organisation's cap counts all its keys; limits for keys belong to a workspace.
		'filter.keys': { hidden: true }
	})
});

export const load = limits.load;

export const actions = {
	...limits.actions,

	/** Clears the counters of a cap: the organisation starts again from zero. */
	reset: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const result = await event.locals.api.POST('/orgs/{org}/limits/{id}/reset', {
			params: { path: { org: event.params.org ?? '', id } }
		});
		if (!result.response.ok) return failed(event, 'reset', result, { id });
		return done('reset', `The counters of limit “${formText(form, 'name')}” were reset.`, { id });
	}
};
