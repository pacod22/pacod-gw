import { csvDownload, USAGE_FILTERS } from '#lib/server/report.ts';
import type { RequestHandler } from './$types';

/** The breakdown as a CSV file, with the filters of the page. */
export const GET: RequestHandler = (event) =>
	csvDownload(
		event,
		'/analytics/breakdown',
		[...USAGE_FILTERS, 'by'],
		{ by: 'model' },
		(q) => `breakdown-by-${q.by}`
	);
