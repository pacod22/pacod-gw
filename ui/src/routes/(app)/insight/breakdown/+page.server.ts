import { loadReport, tagKeysOf, USAGE_FILTERS } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const [report, tagKeys] = await Promise.all([
		loadReport(event, '/analytics/breakdown', [...USAGE_FILTERS, 'by'], { by: 'model' }),
		tagKeysOf(event)
	]);
	return { report, tagKeys, search: event.url.search };
};
