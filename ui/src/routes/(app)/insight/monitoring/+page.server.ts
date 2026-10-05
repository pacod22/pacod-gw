import { problemOf } from '#lib/server/resource.ts';
import { loadReport, USAGE_FILTERS } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const [live, health, alerts, delay] = await Promise.all([
		event.locals.api.GET('/monitoring/live'),
		event.locals.api.GET('/monitoring/health'),
		event.locals.api.GET('/monitoring/alerts'),
		loadReport(event, '/monitoring/delay', [...USAGE_FILTERS, 'bucket', 'group_by'], {
			bucket: 'hour',
			group_by: 'none'
		})
	]);
	return {
		live: live.data ?? null,
		liveProblem: live.response.ok ? null : problemOf(event, live),
		health: health.data ?? [],
		healthProblem: health.response.ok ? null : problemOf(event, health),
		alerts: alerts.data ?? [],
		alertsProblem: alerts.response.ok ? null : problemOf(event, alerts),
		delay
	};
};
