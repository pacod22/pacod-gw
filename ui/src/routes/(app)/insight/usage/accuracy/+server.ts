import { csvDownload, USAGE_FILTERS } from '#lib/server/report.ts';
import type { RequestHandler } from './$types';

/** How much of the cost rests on provider-reported usage, as a CSV file. */
export const GET: RequestHandler = (event) =>
	csvDownload(event, '/analytics/accuracy', USAGE_FILTERS, {}, () => 'cost-accuracy');
