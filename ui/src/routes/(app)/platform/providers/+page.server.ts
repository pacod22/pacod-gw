import { done, failed, formText, resource, type KitEvent } from '#lib/server/resource.ts';

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
export const actions = {
	...providers.actions,

	/** Asks the provider for its model list with the shared credential. */
	probe: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const result = await event.locals.api.POST('/providers/{id}/probe', {
			params: { path: { id } }
		});
		if (!result.response.ok || !result.data) return failed(event, 'probe', result, { id });
		const p = result.data;
		return done(
			'probe',
			`${formText(form, 'name')}: ${p.ok ? 'reachable' : 'not reachable'}. ${p.detail} (${p.latency_ms} ms).`,
			{ id, tone: p.ok ? 'success' : 'warning' }
		);
	}
};
