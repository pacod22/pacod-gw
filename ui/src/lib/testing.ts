/**
 * The Testing pages: what the tester service answers, and how it is shown. The tester's API is
 * not part of the core's OpenAPI document, so its shapes are written down here.
 */

export type Mode = 'mock' | 'live';
export type Need = 'openai' | 'anthropic' | 'gemini' | 'mock_control';
export type CaseStatus = 'pending' | 'running' | 'passed' | 'failed' | 'error' | 'skipped';
export type RunStatus =
	'queued' | 'running' | 'passed' | 'failed' | 'cancelled' | 'error' | 'interrupted';

export interface CatalogCase {
	id: string;
	title: string;
	description: string;
	modes: Mode[];
	needs: Need[];
}

export interface CatalogSuite {
	suite: string;
	title: string;
	description: string;
	source: 'builtin' | 'uploaded';
	fixture: 'none' | 'scratch';
	cases: CatalogCase[];
}

export interface Catalog {
	mock_available: boolean;
	mock_control_available: boolean;
	suites: CatalogSuite[];
}

export interface Counts {
	total: number;
	passed: number;
	failed: number;
	error: number;
	skipped: number;
}

export interface RunSummary {
	id: string;
	status: RunStatus;
	mode: Mode;
	started_by: { id: string; display: string };
	created_at: string;
	started_at: string | null;
	finished_at: string | null;
	params: {
		openai_model: string | null;
		anthropic_model: string | null;
		gemini_model?: string | null;
	};
	stop_on_fail: boolean;
	keep_resources: boolean;
	error: string;
	counts: Counts;
	leftovers: number;
}

export interface StepResult {
	name: string;
	kind: string;
	status: 'passed' | 'failed' | 'error' | 'skipped';
	detail: string;
	failures: string[];
	attempts: number;
	duration_ms: number;
	request: {
		method: string;
		url: string;
		headers: Record<string, string>;
		body: string | null;
	} | null;
	response: {
		status: number;
		headers: Record<string, string>;
		body: string;
		elapsed_ms: number;
	} | null;
}

export interface CaseResult {
	id: string;
	title: string;
	status: CaseStatus;
	reason: string;
	duration_ms: number;
	steps: StepResult[];
}

export interface SuiteResult {
	suite: string;
	title: string;
	source: string;
	status: 'pending' | 'running' | 'done';
	setup: StepResult[];
	cases: CaseResult[];
	teardown: StepResult[];
	leftovers: string[];
}

export interface Run extends Omit<RunSummary, 'leftovers'> {
	suites: SuiteResult[];
}

export const FINISHED: ReadonlySet<RunStatus> = new Set([
	'passed',
	'failed',
	'cancelled',
	'error',
	'interrupted'
]);

export const isFinished = (status: RunStatus): boolean => FINISHED.has(status);

type Tone = 'good' | 'bad' | 'warn' | 'neutral';

/** The badge tone of a run, case or step status. */
export function statusTone(status: string): Tone {
	switch (status) {
		case 'passed':
			return 'good';
		case 'failed':
		case 'error':
			return 'bad';
		case 'running':
		case 'queued':
		case 'interrupted':
			return 'warn';
		default:
			return 'neutral';
	}
}

const NEED_TEXT: Record<Need, string> = {
	openai: 'an OpenAI-dialect model',
	anthropic: 'an Anthropic model',
	gemini: "a model on Gemini's own API",
	mock_control: 'a mock provider it can script'
};

export type Slot = 'openai' | 'anthropic' | 'gemini';

/**
 * The tester's slot a provider's models fill. Mirrors the tester's own rule: every provider
 * managed mode speaks OpenAI's dialect to is in the OpenAI slot, Gemini's own API in its own.
 */
export function slotOf(family: string, managedApi = 'openai_compatible'): Slot | null {
	if (family === 'anthropic') return 'anthropic';
	if (family === 'gemini') return managedApi === 'native' ? 'gemini' : 'openai';
	if (family === 'openai' || family === 'ollama') return 'openai';
	return null;
}

/**
 * Why a case cannot run in a mode with what is set up, or null when it can. Mirrors the
 * tester's own check, so that the form can say it before the run.
 */
export function unavailable(
	c: CatalogCase,
	mode: Mode,
	have: { openai: boolean; anthropic: boolean; gemini: boolean; mockControl: boolean }
): string | null {
	if (!c.modes.includes(mode)) return `runs in ${c.modes.join(' and ')} mode only`;
	const missing = c.needs.filter(
		(n) =>
			(n === 'openai' && !have.openai) ||
			(n === 'anthropic' && !have.anthropic) ||
			(n === 'gemini' && !have.gemini) ||
			(n === 'mock_control' && !have.mockControl)
	);
	return missing.length ? `needs ${missing.map((n) => NEED_TEXT[n]).join(' and ')}` : null;
}

/** A duration in milliseconds, written for people. */
export function formatDuration(ms: number | null | undefined): string {
	if (ms === null || ms === undefined || !Number.isFinite(ms)) return '–';
	if (ms < 1000) return `${Math.round(ms)} ms`;
	if (ms < 60_000) return `${(ms / 1000).toFixed(1)} s`;
	const minutes = Math.floor(ms / 60_000);
	return `${minutes} min ${Math.round((ms % 60_000) / 1000)} s`;
}

/** How long a run took, or has taken so far. */
export function runDuration(run: Pick<RunSummary, 'started_at' | 'finished_at'>, now = Date.now()) {
	if (!run.started_at) return null;
	const end = run.finished_at ? Date.parse(run.finished_at) : now;
	return end - Date.parse(run.started_at);
}

/** "12 passed, 1 failed, 2 skipped": only the counts that are not zero. */
export function countsText(counts: Counts): string {
	const parts = (['passed', 'failed', 'error', 'skipped'] as const)
		.filter((k) => counts[k] > 0)
		.map((k) =>
			k === 'error' ? `${counts[k]} ${counts[k] === 1 ? 'error' : 'errors'}` : `${counts[k]} ${k}`
		);
	return parts.length ? parts.join(', ') : `${counts.total} cases`;
}

/** How many cases have an outcome. */
export const doneCount = (counts: Counts): number =>
	counts.passed + counts.failed + counts.error + counts.skipped;

/** A run id is the tester's: the time it was made and six hex digits. */
export const RUN_ID = /^[0-9]{8}-[0-9]{6}-[0-9a-f]{6}$/;

/** One event of a streamed answer, with when it arrived. */
export interface StreamEvent {
	t_ms: number;
	event: string;
	data: string;
}

/** A message the tester sent to the data plane, and what came back. */
export interface MessageRecord {
	id: string;
	created_at: string;
	started_by: { id: string; display: string };
	org: string;
	ws: string;
	key: { kind: 'temporary' | 'own'; id: string; removed: boolean | null; problem: string };
	method: string;
	path: string;
	url: string;
	request_headers: Record<string, string>;
	body: string | null;
	body_bytes: number;
	plan: {
		status: 'ok' | 'refused' | 'skipped' | 'error';
		detail: string;
		gateway_status: number | null;
		preview: Record<string, unknown> | null;
	};
	exchange: {
		status: number;
		headers: Record<string, string>;
		body: string;
		body_bytes: number;
		truncated: boolean;
		ttfb_ms: number | null;
		first_byte_ms: number | null;
		total_ms: number | null;
		chunks: number;
		stream: boolean;
		events: StreamEvent[];
		error: string;
	};
	request_id: string;
	draft: Record<string, unknown> | null;
}

export interface MessageSummary {
	id: string;
	created_at: string;
	started_by: { id: string; display: string };
	org: string;
	ws: string;
	method: string;
	path: string;
	status: number;
	error: string;
	total_ms: number | null;
	request_id: string;
	model_served: string;
}

/** The gateway's own headers of an answer, in a fixed order, for the trace. */
export function gatewayHeadersOf(headers: Record<string, string>): [string, string][] {
	return Object.entries(headers)
		.filter(([name]) => name.toLowerCase().startsWith('x-gw-'))
		.sort(([a], [b]) => a.localeCompare(b));
}

/** The body of an answer, pretty when it is JSON. */
export function prettyBody(text: string): string {
	try {
		return JSON.stringify(JSON.parse(text), null, 2);
	} catch {
		return text;
	}
}
