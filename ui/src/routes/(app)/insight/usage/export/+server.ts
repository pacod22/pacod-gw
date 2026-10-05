import { csvDownload, USAGE_FILTERS } from '#lib/server/report.ts';
import type { RequestHandler } from './$types';

/** Usage over time as a CSV file, with the filters of the page. */
export const GET: RequestHandler = (event) =>
	csvDownload(
		event,
		'/analytics/usage',
		[...USAGE_FILTERS, 'bucket', 'group_by'],
		{ bucket: 'hour', group_by: 'none' },
		(q) => `usage-per-${q.bucket}${q.group_by && q.group_by !== 'none' ? `-by-${q.group_by}` : ''}`
	);
