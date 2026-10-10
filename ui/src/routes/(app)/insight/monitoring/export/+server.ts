import { csvDownload, USAGE_FILTERS } from '#lib/server/report.ts';
import type { RequestHandler } from './$types';

/** The gateway delay per bucket as a CSV file, with the filters of the page. */
export const GET: RequestHandler = (event) =>
	csvDownload(
		event,
		'/monitoring/delay',
		[...USAGE_FILTERS, 'bucket', 'group_by', 'stage'],
		{ bucket: 'hour', group_by: 'none' },
		(q) => `gateway-delay-per-${q.bucket}${q.stage ? `-${q.stage}` : ''}`
	);
