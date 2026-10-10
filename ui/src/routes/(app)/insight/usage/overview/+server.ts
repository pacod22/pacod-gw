import { csvDownload, SCOPE } from '#lib/server/report.ts';
import type { RequestHandler } from './$types';

/** The current hour, day, week and month, as a CSV file. */
export const GET: RequestHandler = (event) =>
	csvDownload(event, '/analytics/overview', SCOPE, {}, () => 'overview');
