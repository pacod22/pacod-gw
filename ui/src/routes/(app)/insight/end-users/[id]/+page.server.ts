import { loadReport, RANGE, SCOPE } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => ({
	report: await loadReport(event, `/analytics/end-users/${encodeURIComponent(event.params.id)}`, [
		...RANGE,
		...SCOPE
	])
});
