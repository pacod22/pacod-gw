import { resource } from '#lib/server/resource.ts';

const providers = resource({
	path: '/providers',
	noun: 'provider',
	extra: async (event) => {
		// Starting points for the providers the gateway knows.
		const presets = await event.locals.api.GET('/provider-presets');
		return { presets: presets.data ?? [] };
	}
});

export const load = providers.load;
export const actions = providers.actions;
