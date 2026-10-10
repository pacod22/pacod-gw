import { problemOf } from '#lib/server/resource.ts';
import { loadReport, USAGE_FILTERS } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	// All running instances by default; `?instance=` narrows the live figures to one.
	const instance = event.url.searchParams.get('instance')?.trim() || undefined;
	const query = { params: { query: { instance } } };
	const [live, health, alerts, routing, delay] = await Promise.all([
		event.locals.api.GET('/monitoring/live', query),
		event.locals.api.GET('/monitoring/health', query),
		event.locals.api.GET('/monitoring/alerts'),
		event.locals.api.GET('/monitoring/routing', query),
		loadReport(event, '/monitoring/delay', [...USAGE_FILTERS, 'bucket', 'group_by', 'stage'], {
			bucket: 'hour',
			group_by: 'none'
		})
	]);
	return {
		search: event.url.search,
		instance: instance ?? null,
		live: live.data ?? null,
		liveProblem: live.response.ok ? null : problemOf(event, live),
		health: health.data ?? [],
		healthProblem: health.response.ok ? null : problemOf(event, health),
		alerts: alerts.data ?? [],
		alertsProblem: alerts.response.ok ? null : problemOf(event, alerts),
		routing: routing.data ?? [],
		delay
	};
};
