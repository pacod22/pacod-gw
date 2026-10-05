import { allModels, modelOptions } from '#lib/server/options.ts';
import { resource } from '#lib/server/resource.ts';

const grants = resource({
	path: '/orgs/{org}/grants',
	noun: 'grant',
	// A grant only refers to its model: name it by the model.
	nameOf: async (grant, event) =>
		(await allModels(event)).find((m) => m.id === grant.model_id)?.name ?? grant.model_id,
	// Which models an organisation gets is decided by the platform, not by the organisation.
	writers: 'platform',
	fields: async (event) => ({
		model_id: { label: 'Model', options: modelOptions(await allModels(event)) }
	}),
	extra: async (event) => {
		const models = await allModels(event);
		return {
			models: Object.fromEntries(models.map((m) => [m.id, { name: m.name, state: m.state }]))
		};
	}
});

export const load = grants.load;
export const actions = grants.actions;
