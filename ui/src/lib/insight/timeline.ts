import type { Row } from '#lib/report/format.ts';

/** One line of a trace's timeline: a call of a model, of a tool, or of an agent. */
export interface TimelineRow extends Row {
	timestamp: string;
	kind: 'Model' | 'Tool' | 'Agent';
	/** The model served, or the tool as the caller called it, or the agent and the method. */
	call: string;
	/** What came of the call; for a tool call the gateway refused, its verdict. */
	status: string;
	total_ms: number | null;
	/** The model call's id, for its page; tool calls have no page of their own. */
	request_id: string | null;
}

const text = (v: unknown): string => (typeof v === 'string' ? v : '');
const number = (v: unknown): number | null => (typeof v === 'number' ? v : null);

/**
 * The model calls and the tool and agent calls of one trace as one list in time order. Calls
 * at the same instant keep the order they came in, the model calls first. The time stamps are
 * compared as instants: as text, a fraction of a second sorts before the whole second.
 */
export function timeline(requests: Row[], toolCalls: Row[]): TimelineRow[] {
	const models = requests.map((r): TimelineRow => ({
		timestamp: text(r.timestamp),
		kind: 'Model',
		call: text(r.model_served) || text(r.model_requested),
		status: text(r.status),
		total_ms: number(r.total_ms),
		request_id: text(r.id) || null
	}));
	const tools = toolCalls.map((t): TimelineRow => ({
		timestamp: text(t.timestamp),
		kind: t.protocol === 'a2a' ? 'Agent' : 'Tool',
		call: text(t.exposed_name) || [text(t.target), text(t.operation)].filter(Boolean).join(' · '),
		status: t.verdict === 'allowed' ? text(t.status) : text(t.verdict),
		total_ms: number(t.total_ms),
		request_id: null
	}));
	return [...models, ...tools]
		.map((row, index) => ({ row, index, at: Date.parse(row.timestamp) }))
		.sort((a, b) => (a.at || 0) - (b.at || 0) || a.index - b.index)
		.map(({ row }) => row);
}
