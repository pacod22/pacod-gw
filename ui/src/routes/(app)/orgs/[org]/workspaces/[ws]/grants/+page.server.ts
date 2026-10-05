import { allModels, grantedModels, modelOptions } from '#lib/server/options.ts';
import { resource } from '#lib/server/resource.ts';

const grants = resource({
	path: '/orgs/{org}/workspaces/{ws}/grants',
	noun: 'grant',
	// A grant only refers to its model: name it by the model.
	nameOf: async (grant, event) =>
		(await allModels(event)).find((m) => m.id === grant.model_id)?.name ?? grant.model_id,
	// Narrowing a workspace to some models is the organisation's decision.
	writers: 'organisation',
	// Only what the organisation itself was granted can be passed on.
	fields: async (event) => ({
		model_id: {
			label: 'Model',
			options: modelOptions(await grantedModels(event, event.params.org ?? ''))
		}
	}),
	extra: async (event) => {
		const [models, ofOrganisation] = await Promise.all([
			allModels(event),
			grantedModels(event, event.params.org ?? '')
		]);
		return {
			models: Object.fromEntries(models.map((m) => [m.id, { name: m.name, state: m.state }])),
			/** What the workspace may use when it has no grants of its own. */
			inherited: ofOrganisation.map((m) => ({ id: m.id, name: m.name, state: m.state }))
		};
	}
});

export const load = grants.load;
export const actions = grants.actions;
