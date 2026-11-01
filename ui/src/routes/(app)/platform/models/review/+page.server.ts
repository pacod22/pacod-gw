import { error } from '@sveltejs/kit';
import { everyModel } from '#lib/server/options.ts';
import { done, failed, formText, problemOf, type KitEvent } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	if (!event.locals.principal?.platform_admin)
		error(403, 'The review queue is for platform admins.');
	const [proposals, prices, models, job] = await Promise.all([
		event.locals.api.GET('/discovery/proposals', { params: { query: { status: 'open' } } }),
		event.locals.api.GET('/price-proposals'),
		everyModel(event.locals.api),
		event.locals.api.GET('/price-job')
	]);
	return {
		proposals: proposals.data ?? [],
		prices: prices.data ?? [],
		models: models.map((m) => ({
			id: m.id,
			name: m.name,
			provider_id: m.provider_id,
			provider: m.provider,
			upstream_id: m.upstream_id,
			state: m.state,
			priced: m.priced,
			free: m.free ?? false,
			created_at: m.created_at
		})),
		// Without the job the page cannot say whether the tracker looks for a price: it says so.
		job: job.data ? { enabled: job.data.enabled, sources: job.data.sources } : null,
		problem: !proposals.response.ok
			? problemOf(event, proposals)
			: !prices.response.ok
				? problemOf(event, prices)
				: null
	};
};

async function decidePrice(event: KitEvent, decision: 'approve' | 'reject') {
	const form = await event.request.formData();
	const id = formText(form, 'id');
	const result = await event.locals.api.POST(`/price-proposals/{id}/${decision}`, {
		params: { path: { id } }
	});
	if (!result.response.ok || !result.data) return failed(event, decision, result, { id });
	return done(
		decision,
		decision === 'approve'
			? `The price of “${result.data.model}” is in effect as version ${result.data.version}.`
			: `The proposed price of “${result.data.model}” was rejected.`,
		{ id }
	);
}

export const actions = {
	approve: (event: KitEvent) => decidePrice(event, 'approve'),
	reject: (event: KitEvent) => decidePrice(event, 'reject'),

	/** Dismisses a discovery proposal: a later run does not propose it again. */
	dismiss: async (event: KitEvent) => {
		const form = await event.request.formData();
		const id = formText(form, 'id');
		const result = await event.locals.api.POST('/discovery/proposals/{id}/dismiss', {
			params: { path: { id } }
		});
		if (!result.response.ok) return failed(event, 'dismiss', result, { id });
		return done('dismiss', 'Dismissed. A later discovery run does not propose it again.', { id });
	}
};
