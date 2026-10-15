/**
 * Load tests: what the tester's load routes answer, the request the form builds, and the
 * series the charts draw. The tester is not in the OpenAPI document; these types follow its
 * models (`tester/src/pgw_tester/load/model.py`).
 */
import type { ChartSeries } from './report/stack';
import type { StepResult } from './testing';

export type LoadTarget = 'mock' | 'live' | 'own' | 'external';
export type LoadInterface = 'openai' | 'anthropic' | 'gemini' | 'native' | 'embeddings';
export type LoadStatus =
	| 'queued'
	| 'preparing'
	| 'running'
	| 'draining'
	| 'cleaning'
	| 'finished'
	| 'cancelled'
	| 'error'
	| 'interrupted';

export const LOAD_FINISHED: ReadonlySet<LoadStatus> = new Set([
	'finished',
	'cancelled',
	'error',
	'interrupted'
]);
export const isLoadFinished = (status: LoadStatus): boolean => LOAD_FINISHED.has(status);

export interface Stage {
	duration_s: number;
	target: number;
}

export interface AgentCaps {
	max_concurrency: number;
	max_rate: number;
	max_duration_s: number;
	max_body_bytes: number;
}

/** An agent as `GET /load/agents` shows it: asked just now whether it is there. */
export interface AgentView {
	name: string;
	url: string;
	reachable: boolean;
	error: string;
	version?: string;
	target?: string;
	cpus?: number;
	caps?: AgentCaps;
	busy?: { run_id: string; state: string } | null;
}

export interface AgentsAnswer {
	configured: boolean;
	agents: AgentView[];
	mock_available: boolean;
	live_max_calls: number;
	max_duration: number;
	active: string | null;
	/** The id of the test of either kind that holds the tester. */
	busy: string | null;
}

/** Milliseconds. */
export interface Pct {
	p50: number;
	p95: number;
	p99: number;
	max: number;
}

/** One second of a load test, all agents together. */
export interface Point {
	t: number;
	target: number;
	started: number;
	completed: number;
	ok: number;
	failed: number;
	cancelled: number;
	status: Record<string, number>;
	bytes: number;
	in_flight: number;
	skipped: number;
	ttfb: Pct | null;
	total: Pct | null;
	gw_delay: Pct | null;
	cpu_max: number | null;
	lag_max_ms: number;
	gw_in_flight: number | null;
	agents: number;
}

export interface GatewaySample {
	t: number;
	in_flight: number;
	requests_per_second: number;
	rejected_per_second: number;
	spool_bytes: number;
	events_dropped: number;
}

export interface Probe {
	status: number | null;
	ttfb_ms: number;
	total_ms: number;
	error: string | null;
	body_excerpt: string;
}

export interface AgentPart {
	name: string;
	url: string;
	version: string;
	target: string;
	cpus: number;
	share: { stages: Stage[]; max_in_flight: number | null; max_calls: number | null } | null;
	state: string;
	ended: string | null;
	error: string;
	probe: Probe | null;
	calls: number;
	cpu_max: number;
	lag_max_ms: number;
}

export interface Errors {
	connect: number;
	timeout: number;
	body: number;
	other: number;
	cancelled: number;
}

export interface LoadSummary {
	duration_s: number;
	started: number;
	calls: number;
	ok: number;
	failed: number;
	cancelled: number;
	skipped: number;
	status: Record<string, number>;
	errors: Errors;
	bytes: number;
	rps_mean: number;
	rps_peak: number;
	ttfb: Pct;
	total: Pct;
	gw_delay: Pct | null;
}

export interface Scene {
	org: string;
	ws: string;
	key_id: string;
	model: string;
	model_id: string;
	provider: string;
	upstream: string;
	profile: string;
	ledger: string[];
	setup: StepResult[];
	teardown: StepResult[];
	leftovers: string[];
}

/** The request as the run keeps it: a pasted key is reduced to a hint. */
export interface StoredRequest {
	target: LoadTarget;
	interface: LoadInterface;
	mode: 'managed' | 'proxy';
	stream: boolean;
	prompt_words: number;
	max_tokens: number;
	http: '1.1' | '2';
	mock: { family: string; delay_ms: number; chunk_delay_ms: number; chunks: number } | null;
	live: { model_id: string } | null;
	own: { model: string; key_hint: string; provider: string; upstream: string } | null;
	external: {
		model: string;
		key_hint: string;
		base_url: string;
		provider: string;
		upstream: string;
	} | null;
	load: {
		mode: 'concurrency' | 'rate';
		stages: Stage[];
		max_in_flight: number | null;
		timeout_s: number;
		max_calls: number | null;
	};
	agents: string[];
	keep_resources: boolean;
}

interface LoadBase {
	id: string;
	status: LoadStatus;
	label: string;
	target: LoadTarget;
	started_by: { id: string; display: string };
	created_at: string;
	started_at: string | null;
	finished_at: string | null;
	request: StoredRequest;
	base_url: string;
	summary: LoadSummary | null;
	warnings: string[];
	error: string;
}

export interface LoadRunSummary extends LoadBase {
	agents: { name: string; state: string }[];
	leftovers: number;
}

export interface LoadRun extends LoadBase {
	agents: AgentPart[];
	scene: Scene;
	points: Point[];
	gateway: GatewaySample[];
}

type Tone = 'good' | 'bad' | 'warn' | 'neutral';

export function loadStatusTone(status: string): Tone {
	switch (status) {
		case 'finished':
		case 'done':
			return 'good';
		case 'error':
		case 'failed':
		case 'lost':
			return 'bad';
		case 'cancelled':
		case 'interrupted':
		case 'neutral':
			return 'neutral';
		default:
			return 'warn';
	}
}

export const TARGET_TEXT: Record<LoadTarget, string> = {
	mock: 'Mock provider',
	live: 'Live model',
	own: 'Own key',
	external: 'External gateway'
};

export const INTERFACES: { id: LoadInterface; label: string }[] = [
	{ id: 'openai', label: 'OpenAI chat completions' },
	{ id: 'anthropic', label: 'Anthropic messages' },
	{ id: 'gemini', label: "Gemini's own API" },
	{ id: 'native', label: 'Native chat (managed mode only)' },
	{ id: 'embeddings', label: 'Embeddings' }
];

export const totalDuration = (stages: Stage[]): number =>
	stages.reduce((n, s) => n + (Number.isFinite(s.duration_s) ? s.duration_s : 0), 0);

export const peakOf = (stages: Stage[]): number =>
	stages.reduce((n, s) => Math.max(n, Number.isFinite(s.target) ? s.target : 0), 0);

/** The shapes the form offers to start from. */
export function preset(
	kind: 'steady' | 'ramp' | 'steps',
	target: number,
	seconds: number
): Stage[] {
	const t = Math.max(1, target);
	const s = Math.max(1, seconds);
	switch (kind) {
		case 'steady':
			return [
				{ duration_s: 0, target: t },
				{ duration_s: s, target: t }
			];
		case 'ramp':
			return [
				{ duration_s: Math.max(1, Math.round(s / 4)), target: t },
				{ duration_s: s, target: t },
				{ duration_s: Math.max(1, Math.round(s / 8)), target: 0 }
			];
		case 'steps': {
			const step = Math.max(1, Math.round(s / 4));
			return [1, 2, 3, 4].flatMap((k) => [
				{ duration_s: 0, target: Math.round((t * k) / 4) },
				{ duration_s: step, target: Math.round((t * k) / 4) }
			]);
		}
	}
}

/** Seconds as `1:05` (or `1:02:05`). */
export function clock(seconds: number): string {
	const s = Math.max(0, Math.round(seconds));
	const h = Math.floor(s / 3600);
	const m = Math.floor((s % 3600) / 60);
	const r = String(s % 60).padStart(2, '0');
	return h ? `${h}:${String(m).padStart(2, '0')}:${r}` : `${m}:${r}`;
}

/** Milliseconds, with the precision a latency needs. */
export function ms(value: number | null | undefined): string {
	if (value === null || value === undefined || !Number.isFinite(value)) return '–';
	if (value < 10) return `${value.toFixed(2)} ms`;
	if (value < 1000) return `${value.toFixed(0)} ms`;
	return `${(value / 1000).toFixed(2)} s`;
}

// --- the form ---------------------------------------------------------------------------------

const text = (form: FormData, name: string): string => String(form.get(name) ?? '').trim();

function whole(form: FormData, name: string, label: string, min: number, max: number): number {
	const raw = text(form, name);
	const n = Number(raw);
	if (raw === '' || !Number.isInteger(n) || n < min || n > max)
		throw new Error(`${label} is a whole number from ${min} to ${max}.`);
	return n;
}

function optionalWhole(form: FormData, name: string, label: string, min: number): number | null {
	if (text(form, name) === '') return null;
	return whole(form, name, label, min, Number.MAX_SAFE_INTEGER);
}

/**
 * The tester's load request from the form. The tester checks everything again; this answers
 * the plain mistakes with a sentence instead of a list of field paths.
 */
export function loadRequestFrom(
	form: FormData
): { request: Record<string, unknown>; error: null } | { request: null; error: string } {
	try {
		const target = text(form, 'target') as LoadTarget;
		if (!['mock', 'live', 'own', 'external'].includes(target)) throw new Error('Choose a target.');
		const iface = text(form, 'interface') || 'openai';
		const mode = text(form, 'mode') === 'proxy' ? 'proxy' : 'managed';
		const loadMode = text(form, 'load_mode') === 'rate' ? 'rate' : 'concurrency';
		const durations = form.getAll('stage_duration').map((v) => String(v).trim());
		const targets = form.getAll('stage_target').map((v) => String(v).trim());
		const stages: Stage[] = durations.map((d, i) => ({
			duration_s: Number(d),
			target: Number(targets[i] ?? '')
		}));
		if (stages.length === 0) throw new Error('Add at least one stage.');
		for (const s of stages) {
			if (!Number.isInteger(s.duration_s) || s.duration_s < 0)
				throw new Error('A stage lasts a whole number of seconds, 0 or more.');
			if (!Number.isFinite(s.target) || s.target < 0)
				throw new Error("A stage's target is a number, 0 or more.");
			if (loadMode === 'concurrency') s.target = Math.round(s.target);
		}
		if (totalDuration(stages) <= 0) throw new Error('The stages last no time at all.');
		const agents = form.getAll('agent').map(String).filter(Boolean);
		if (agents.length === 0) throw new Error('Choose at least one agent.');

		const request: Record<string, unknown> = {
			label: text(form, 'label'),
			target,
			interface: iface,
			mode,
			stream: form.get('stream') === 'on',
			prompt_words: whole(form, 'prompt_words', 'The prompt', 1, 20_000),
			max_tokens: whole(form, 'max_tokens', 'The answer', 1, 100_000),
			http: text(form, 'http') === '2' ? '2' : '1.1',
			load: {
				mode: loadMode,
				stages,
				max_in_flight:
					loadMode === 'rate'
						? whole(form, 'max_in_flight', 'Calls in flight', 1, 1_000_000)
						: null,
				timeout_s: whole(form, 'timeout_s', 'The timeout', 1, 600),
				max_calls: optionalWhole(form, 'max_calls', 'The number of calls', 1)
			},
			agents,
			acknowledge_cost: form.get('acknowledge_cost') === 'on',
			keep_resources: form.get('keep_resources') === 'on'
		};
		if (target === 'mock')
			request.mock = {
				family: text(form, 'mock_family') || 'openai',
				delay_ms: whole(form, 'mock_delay', 'The delay', 0, 60_000),
				chunk_delay_ms: whole(form, 'mock_chunk_delay', 'The delay between chunks', 0, 10_000),
				chunks: whole(form, 'mock_chunks', 'The number of chunks', 1, 1000)
			};
		if (target === 'live') {
			const model = text(form, 'live_model');
			if (!model) throw new Error('Choose the model.');
			request.live = { model_id: model };
		}
		if (target === 'own' || target === 'external') {
			const secret = text(form, `${target}_secret`);
			const model = text(form, `${target}_model`);
			if (!secret) throw new Error('Paste the key the calls carry.');
			if (!model) throw new Error('Name the model as the gateway knows it.');
			const keyed: Record<string, unknown> = {
				secret,
				model,
				provider: text(form, `${target}_provider`),
				upstream: text(form, `${target}_upstream`),
				mock_options: text(form, `${target}_mock_options`)
			};
			if (target === 'external') {
				const url = text(form, 'external_url');
				if (!/^https?:\/\/[^\s/]+/.test(url))
					throw new Error("Give the gateway's URL, http:// or https:// with a host.");
				keyed.base_url = url;
			}
			request[target] = keyed;
		}
		return { request, error: null };
	} catch (e) {
		return { request: null, error: e instanceof Error ? e.message : String(e) };
	}
}

// --- the charts -------------------------------------------------------------------------------

export interface LoadChart {
	title: string;
	labels: string[];
	series: ChartSeries[];
	format: (value: number) => string;
	/** Says what the chart shows when it has nothing yet. */
	empty: string;
}

const pick = (p: Pct | null, k: keyof Pct): number | null => (p ? p[k] : null);
const count = (v: number) => (Number.isInteger(v) ? v.toLocaleString('en-GB') : v.toFixed(1));

/** The live charts of a load test, from its points. */
export function chartsOf(
	points: Point[],
	target: LoadTarget,
	mode: 'concurrency' | 'rate'
): LoadChart[] {
	const labels = points.map((p) => clock(p.t));
	const latency = (
		title: string,
		key: 'ttfb' | 'total' | 'gw_delay',
		empty: string
	): LoadChart => ({
		title,
		labels,
		series: [
			{ name: 'p50', values: points.map((p) => pick(p[key], 'p50')), colour: '--series-1' },
			{ name: 'p95', values: points.map((p) => pick(p[key], 'p95')), colour: '--series-3' },
			{ name: 'p99', values: points.map((p) => pick(p[key], 'p99')), colour: '--series-5' }
		],
		format: ms,
		empty
	});
	const charts: LoadChart[] = [
		{
			title: 'Calls a second',
			labels,
			series: [
				{ name: 'Successful', values: points.map((p) => p.ok), colour: '--series-1' },
				...(mode === 'rate'
					? [{ name: 'Target', values: points.map((p) => p.target), colour: '--series-4' }]
					: []),
				{ name: 'Failed', values: points.map((p) => p.failed), colour: '--danger', bar: true },
				{
					name: 'Not started',
					values: points.map((p) => p.skipped),
					colour: '--warning',
					bar: true
				}
			],
			format: count,
			empty: 'No call has finished yet.'
		},
		latency('Time to the first byte', 'ttfb', 'No successful call yet.'),
		latency('Whole call', 'total', 'No successful call yet.'),
		latency(
			"The gateway's own delay",
			'gw_delay',
			'The target sends no x-gw-delay-ms: it is not pacod-gw, or no call has succeeded yet.'
		),
		{
			title: 'Calls in flight',
			labels,
			series: [
				{ name: 'At the agents', values: points.map((p) => p.in_flight), colour: '--series-1' },
				...(target === 'external'
					? []
					: [
							{
								name: 'At the gateway',
								values: points.map((p) => p.gw_in_flight),
								colour: '--series-6'
							}
						]),
				...(mode === 'concurrency'
					? [{ name: 'Target', values: points.map((p) => p.target), colour: '--series-4' }]
					: [])
			],
			format: count,
			empty: 'Nothing in flight yet.'
		},
		{
			title: 'The agents',
			labels,
			series: [
				{
					name: 'Busiest agent, % of its CPUs',
					values: points.map((p) => p.cpu_max),
					colour: '--series-2'
				},
				{ name: 'Scheduler lag, ms', values: points.map((p) => p.lag_max_ms), colour: '--series-7' }
			],
			format: (v) => (v >= 10 ? v.toFixed(0) : v.toFixed(1)),
			empty: 'The agents have not reported yet.'
		}
	];
	return charts;
}

/** What the points add up to while the run goes (the summary replaces it at the end). */
export function runningTotals(points: Point[]) {
	const sum = (f: (p: Point) => number) => points.reduce((n, p) => n + f(p), 0);
	const last = points.at(-1) ?? null;
	return {
		calls: sum((p) => p.completed),
		ok: sum((p) => p.ok),
		failed: sum((p) => p.failed),
		skipped: sum((p) => p.skipped),
		rate: last ? last.completed : 0,
		ttfbP50: last?.ttfb?.p50 ?? null,
		ttfbP99: last?.ttfb?.p99 ?? null,
		cpu: points.reduce((n, p) => Math.max(n, p.cpu_max ?? 0), 0)
	};
}
