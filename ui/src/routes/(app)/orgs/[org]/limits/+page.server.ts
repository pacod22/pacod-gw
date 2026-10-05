import { grantedModels, modelOptions } from '#lib/server/options.ts';
import { resource } from '#lib/server/resource.ts';

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
		'filter.tags': { label: 'Only calls with these tags' }
	})
});

export const load = limits.load;
export const actions = limits.actions;
