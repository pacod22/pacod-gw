import { error } from '@sveltejs/kit';
import { done, failed, formText, problemOf, type KitEvent } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	if (!event.locals.principal?.platform_admin)
		error(403, 'Model discovery is for platform admins.');
	const [proposals, runs, presets] = await Promise.all([
		event.locals.api.GET('/discovery/proposals', { params: { query: { status: 'open' } } }),
		event.locals.api.GET('/discovery/runs'),
		event.locals.api.GET('/capability-presets')
	]);
	return {
		proposals: proposals.data ?? [],
		runs: runs.data ?? [],
		presets: (presets.data ?? []).map((p) => ({ name: p.name, description: p.description })),
		problem: proposals.response.ok ? null : problemOf(event, proposals)
	};
};

export const actions = {
	/** Asks for a run; a management instance starts it within half a minute. */
	run: async (event: KitEvent) => {
		const result = await event.locals.api.POST('/discovery/runs');
		if (!result.response.ok) return failed(event, 'run', result);
		return done(
			'run',
			'Discovery starts within half a minute. Reload the page to see what it finds.'
		);
	},

	/** Creates a draft model from a proposal. */
	accept: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const naming = formText(form, 'naming') === 'bare' ? 'bare' : 'prefixed';
		const preset = formText(form, 'capability_preset');
		const kind = formText(form, 'kind');
		const result = await event.locals.api.POST('/discovery/proposals/{id}/accept', {
			params: { path: { id } },
			body: {
				name: null,
				naming,
				capability_preset: preset || null,
				kind:
					kind === 'chat' || kind === 'speech' || kind === 'embedding' || kind === 'decision'
						? kind
						: null
			}
		});
		if (!result.response.ok || !result.data) return failed(event, 'accept', result, { id });
		return done(
			'accept',
			`Draft model “${result.data.name}” created. Give it a price or mark it free, check its capabilities, then activate it.`,
			{ id }
		);
	},

	dismiss: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const result = await event.locals.api.POST('/discovery/proposals/{id}/dismiss', {
			params: { path: { id } }
		});
		if (!result.response.ok) return failed(event, 'dismiss', result, { id });
		return done('dismiss', 'Dismissed. A later run does not propose it again.', { id });
	}
};
