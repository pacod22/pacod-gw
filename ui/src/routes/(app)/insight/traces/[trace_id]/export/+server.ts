import { csvDownload, SCOPE } from '#lib/server/report.ts';
import type { RequestHandler } from './$types';

/** The calls of one trace as a CSV file. */
export const GET: RequestHandler = (event) =>
	csvDownload(
		event,
		`/analytics/traces/${encodeURIComponent(event.params.trace_id)}`,
		SCOPE,
		{},
		() => `trace-${event.params.trace_id}`
	);
