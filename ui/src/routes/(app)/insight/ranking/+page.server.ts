import { loadReport, tagKeysOf, USAGE_FILTERS } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

/** The parameters of the ranking besides the usage filters. */
const RANKING = ['by', 'kind', 'preset', 'min_requests'];

export const load: PageServerLoad = async (event) => {
	const [report, tagKeys] = await Promise.all([
		loadReport(event, '/analytics/ranking', [...USAGE_FILTERS, ...RANKING], {
			by: 'model',
			kind: 'chat',
			preset: 'balanced'
		}),
		tagKeysOf(event)
	]);
	return { report, tagKeys, search: event.url.search };
};
