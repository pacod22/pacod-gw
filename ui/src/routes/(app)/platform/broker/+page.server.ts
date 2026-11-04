import { error } from '@sveltejs/kit';
import { hasPlatformRole } from '#lib/access.ts';
import { problemOf } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

/** How many linked models the page asks the broker about at once. */
const MAX_LINKED = 50;

export interface LinkedModel {
	id: string;
	name: string;
	state: string;
	resource: string;
	score: number | null;
	label: string;
	price: string;
	live: string;
}

function priceOf(v: Record<string, unknown> | undefined): string {
	const p = v?.price_now as
		{ currency?: string; rates?: Record<string, number> } | null | undefined;
	if (!p?.rates) return '';
	const r = p.rates;
	const parts = [
		r.input !== undefined ? `in ${r.input}` : '',
		r.output !== undefined ? `out ${r.output}` : ''
	]
		.filter(Boolean)
		.join(' · ');
	return parts ? `${p.currency ?? ''} ${parts}` : '';
}

export const load: PageServerLoad = async (event) => {
	const principal = event.locals.principal;
	if (!principal || !hasPlatformRole(principal)) {
		error(403, 'The broker page is for platform roles.');
	}
	const status = await event.locals.api.GET('/broker/status');
	const models = await event.locals.api.GET('/models', { params: { query: { limit: 500 } } });
	const items = (
		(models.data as { items?: Record<string, unknown>[] } | undefined)?.items ?? []
	).filter((m) => typeof m.broker_resource_id === 'string' && m.broker_resource_id !== '');
	const configured = Boolean((status.data as { configured?: boolean } | undefined)?.configured);
	const linked: LinkedModel[] = await Promise.all(
		items.slice(0, MAX_LINKED).map(async (m) => {
			const resource = String(m.broker_resource_id);
			const base: LinkedModel = {
				id: String(m.id),
				name: String(m.name),
				state: String(m.state ?? ''),
				resource,
				score: null,
				label: '',
				price: '',
				live: ''
			};
			if (!configured) return base;
			const r = await event.locals.api.GET('/broker/resources/{id}', {
				params: { path: { id: resource } }
			});
			const v = r.data as Record<string, unknown> | undefined;
			if (!r.response.ok || !v) return { ...base, live: 'not on the broker now' };
			const rep = v.reputation as { score?: number; label?: string } | null | undefined;
			const live = v.live as { status?: string } | undefined;
			return {
				...base,
				score: typeof rep?.score === 'number' ? rep.score : null,
				label: rep?.label ?? 'new',
				price: priceOf(v),
				live: live?.status ?? ''
			};
		})
	);
	return {
		status: status.data ?? null,
		problem: status.response.ok ? null : problemOf(event, status),
		linked,
		more: Math.max(0, items.length - MAX_LINKED)
	};
};
