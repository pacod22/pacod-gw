import {
	grantedModels,
	modelOptions,
	templateOptions,
	toolBundleOptions
} from '#lib/server/options.ts';
import { resource } from '#lib/server/resource.ts';

const agents = resource({
	path: '/orgs/{org}/workspaces/{ws}/agents',
	noun: 'agent',
	fields: async (event) => {
		const org = event.params.org ?? '';
		const ws = event.params.ws ?? '';
		const [models, templates, bundles] = await Promise.all([
			grantedModels(event, org, ws),
			templateOptions(event, org, ws),
			toolBundleOptions(event, org, ws)
		]);
		return {
			name: { help: 'The agent_id its calls carry in every report. It cannot change.' },
			allowed_models: {
				help: 'Within what its keys may use. None chosen narrows nothing.',
				options: modelOptions(models, 'name')
			},
			allowed_templates: {
				help: 'Within what its keys may use. None chosen narrows nothing.',
				options: templates
			},
			tool_bundles: {
				help: 'Within its keys’ bundles. None chosen narrows nothing.',
				options: bundles
			},
			status: { help: 'Disabled stops every call its keys make for it, within a second.' }
		};
	}
});

export const load = agents.load;
export const actions = agents.actions;
