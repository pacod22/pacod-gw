import { error } from '@sveltejs/kit';
import { done, failed, formText, problemOf, type KitEvent } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	if (!event.locals.principal?.platform_admin)
		error(403, 'Model discovery is for platform admins.');
	const [proposals, runs, presets, settings] = await Promise.all([
		event.locals.api.GET('/discovery/proposals', { params: { query: { status: 'open' } } }),
		event.locals.api.GET('/discovery/runs'),
		event.locals.api.GET('/capability-presets'),
		event.locals.api.GET('/settings')
	]);
	return {
		proposals: proposals.data ?? [],
		runs: runs.data ?? [],
		presets: (presets.data ?? []).map((p) => ({
			name: p.name,
			family: p.family,
			description: p.description
		})),
		// Without the settings the page leaves the schedule out rather than guess it.
		intervalHours: settings.data?.discovery_interval_hours ?? null,
		problem: proposals.response.ok ? null : problemOf(event, proposals)
	};
};

const KINDS = [
	'chat',
	'speech',
	'embedding',
	'decision',
	'image',
	'transcription',
	'rerank',
	'realtime'
] as const;

export const actions = {
	/** Asks for a run; a management instance starts it within half a minute. */
	run: async (event: KitEvent) => {
		const result = await event.locals.api.POST('/discovery/runs');
		if (!result.response.ok) return failed(event, 'run', result);
		return done('run', 'Discovery starts within half a minute. This page updates when it is done.');
	},

	/** Creates a draft model from a proposal. */
	accept: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const naming = formText(form, 'naming');
		const kind = KINDS.find((k) => k === formText(form, 'kind')) ?? null;
		const preset = formText(form, 'capability_preset');
		const result = await event.locals.api.POST('/discovery/proposals/{id}/accept', {
			params: { path: { id } },
			body: {
				name: naming === 'custom' ? formText(form, 'name').trim() : null,
				naming: naming === 'bare' ? 'bare' : 'prefixed',
				// Only a preset other than the suggested one is sent as chosen: the core lets what the
				// provider reported about images override a suggested preset, not a chosen one.
				capability_preset: preset && preset !== formText(form, 'suggested_preset') ? preset : null,
				kind
			}
		});
		if (!result.response.ok || !result.data) return failed(event, 'accept', result, { id });
		return done('accept', `Draft model “${result.data.name}” created.`, {
			id,
			data: { model_id: result.data.id, name: result.data.name }
		});
	},

	/**
	 * Dismisses one proposal (`id`) or several (`ids`, separated by spaces). A proposal someone
	 * else decided on in the meantime is counted, not reported as a failure.
	 */
	dismiss: async (event: KitEvent) => {
		const form = await event.request.formData();
		const ids = [formText(form, 'id'), ...formText(form, 'ids').split(/\s+/)].filter(Boolean);
		let dismissed = 0;
		let closed = 0;
		for (const id of ids) {
			const result = await event.locals.api.POST('/discovery/proposals/{id}/dismiss', {
				params: { path: { id } }
			});
			if (result.response.ok) dismissed += 1;
			else if (result.response.status === 409) closed += 1;
			else return failed(event, 'dismiss', result, ids.length === 1 ? { id } : {});
		}
		const what = dismissed === 1 ? 'one proposal' : `${dismissed} proposals`;
		return done(
			'dismiss',
			`Dismissed ${what}. A later run does not propose ${dismissed === 1 ? 'it' : 'them'} again.` +
				(closed > 0 ? ` ${closed} had been decided already.` : ''),
			{ tone: closed > 0 ? 'warning' : 'success' }
		);
	}
};
