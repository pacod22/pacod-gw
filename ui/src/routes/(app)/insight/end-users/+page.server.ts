import { loadReport, RANGE, SCOPE } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => ({
	report: await loadReport(
		event,
		'/analytics/end-users',
		[...RANGE, ...SCOPE, 'model', 'template', 'key_id', 'limit', 'offset'],
		{ limit: '50', offset: '0' }
	),
	search: event.url.search
});
