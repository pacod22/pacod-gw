import { csvDownload, RANGE, SCOPE } from '#lib/server/report.ts';
import type { RequestHandler } from './$types';

/** The end users with their consumption as a CSV file, with the filters of the page. */
export const GET: RequestHandler = (event) =>
	csvDownload(
		event,
		'/analytics/end-users',
		[...RANGE, ...SCOPE, 'model', 'template', 'key_id', 'limit', 'offset'],
		{ limit: '1000', offset: '0' },
		() => 'end-users'
	);
