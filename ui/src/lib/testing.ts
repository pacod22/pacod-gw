/**
 * The Testing pages: what the tester service answers, and how it is shown. The tester's API is
 * not part of the core's OpenAPI document, so its shapes are written down here.
 */

export type Mode = 'mock' | 'live';
export type Need = 'openai' | 'anthropic' | 'mock_control';
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
	params: { openai_model: string | null; anthropic_model: string | null };
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
	mock_control: 'a mock provider it can script'
};

/**
 * Why a case cannot run in a mode with what is set up, or null when it can. Mirrors the
 * tester's own check, so that the form can say it before the run.
 */
export function unavailable(
	c: CatalogCase,
	mode: Mode,
	have: { openai: boolean; anthropic: boolean; mockControl: boolean }
): string | null {
	if (!c.modes.includes(mode)) return `runs in ${c.modes.join(' and ')} mode only`;
	const missing = c.needs.filter(
		(n) =>
			(n === 'openai' && !have.openai) ||
			(n === 'anthropic' && !have.anthropic) ||
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
