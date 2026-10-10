import { csvDownload, USAGE_FILTERS } from '#lib/server/report.ts';
import type { RequestHandler } from './$types';

/** The failures per provider and model as a CSV file, with the filters of the page. */
export const GET: RequestHandler = (event) =>
	csvDownload(event, '/errors', USAGE_FILTERS, {}, () => 'errors-by-model');
