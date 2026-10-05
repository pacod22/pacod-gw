import { loadReport, SCOPE } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => ({
	report: await loadReport(
		event,
		`/analytics/traces/${encodeURIComponent(event.params.trace_id)}`,
		SCOPE
	)
});
