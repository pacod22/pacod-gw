import { csvDownload, USAGE_FILTERS } from '#lib/server/report.ts';
import type { RequestHandler } from './$types';

/** The ranking as a CSV file, with the filters of the page. */
export const GET: RequestHandler = (event) =>
	csvDownload(
		event,
		'/analytics/ranking',
		[...USAGE_FILTERS, 'by', 'kind', 'preset', 'min_requests'],
		{ by: 'model', kind: 'chat', preset: 'balanced' },
		(q) => `ranking-${q.kind}-by-${q.by}`
	);
