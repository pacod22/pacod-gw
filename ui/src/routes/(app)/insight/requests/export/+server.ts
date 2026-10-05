import { queryParams } from '#lib/schema/spec.ts';
import { csvDownload } from '#lib/server/report.ts';
import { spec } from '#lib/server/spec.ts';
import type { RequestHandler } from './$types';

// The filters the request log declares, without paging: a download is one file.
const FILTERS = queryParams(spec, '/requests').filter(
	(name) => name !== 'cursor' && name !== 'limit'
);

/** The request log as a CSV file, with the filters of the page: the newest 1,000 calls they select. */
export const GET: RequestHandler = (event) =>
	csvDownload(
		event,
		'/requests',
		[...FILTERS, 'tag.{key}', 'limit'],
		{ limit: '1000' },
		() => 'requests'
	);
