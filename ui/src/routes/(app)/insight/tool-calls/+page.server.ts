import { loadReport, RANGE, SCOPE } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

/** The filters the analytics service takes on the records of tool and agent calls. */
const TOOL_FILTERS = [
	'key_id',
	'end_user',
	'session_id',
	'trace_id',
	'agent_id',
	'user_id',
	'task',
	'protocol',
	'target',
	'operation',
	'tool',
	'verdict',
	'status'
];

export const load: PageServerLoad = async (event) => ({
	report: await loadReport(event, '/tool-calls', [...RANGE, ...SCOPE, ...TOOL_FILTERS, 'cursor']),
	cursor: event.url.searchParams.get('cursor')
});
