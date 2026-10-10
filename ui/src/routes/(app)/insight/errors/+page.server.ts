import { loadReport, USAGE_FILTERS } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => ({
	report: await loadReport(event, '/errors', USAGE_FILTERS),
	search: event.url.search
});
