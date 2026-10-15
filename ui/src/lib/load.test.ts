import { describe, expect, it } from 'vitest';
import {
	chartsOf,
	clock,
	loadRequestFrom,
	ms,
	peakOf,
	preset,
	runningTotals,
	totalDuration,
	type Point
} from './load';

/** The parts of a built request the tests read. */
interface Built {
	mock: unknown;
	external: { base_url: string; secret: string };
	load: { stages: unknown; max_calls: number | null };
	agents: string[];
	stream: boolean;
	acknowledge_cost: boolean;
}

function form(fields: [string, string][]): FormData {
	const f = new FormData();
	for (const [k, v] of fields) f.append(k, v);
	return f;
}

const BASE: [string, string][] = [
	['interface', 'openai'],
	['mode', 'managed'],
	['stream', 'on'],
	['prompt_words', '60'],
	['max_tokens', '64'],
	['http', '1.1'],
	['load_mode', 'concurrency'],
	['stage_duration', '0'],
	['stage_target', '10.4'],
	['stage_duration', '30'],
	['stage_target', '10'],
	['timeout_s', '120'],
	['max_calls', ''],
	['agent', 'local'],
	['agent', 'remote']
];

describe('the load request from the form', () => {
	it('builds a mock request, whole calls at a time', () => {
		const r = loadRequestFrom(
			form([
				['target', 'mock'],
				['mock_family', 'anthropic'],
				['mock_delay', '200'],
				['mock_chunk_delay', '20'],
				['mock_chunks', '20'],
				...BASE
			])
		);
		expect(r.error).toBeNull();
		const req = r.request as unknown as Built;
		expect(req.mock).toEqual({
			family: 'anthropic',
			delay_ms: 200,
			chunk_delay_ms: 20,
			chunks: 20
		});
		expect(req.load.stages).toEqual([
			{ duration_s: 0, target: 10 },
			{ duration_s: 30, target: 10 }
		]);
		expect(req.load.max_calls).toBeNull();
		expect(req.agents).toEqual(['local', 'remote']);
		expect(req.stream).toBe(true);
	});

	it('builds an external request with the URL and the key', () => {
		const r = loadRequestFrom(
			form([
				['target', 'external'],
				['external_url', 'http://host.docker.internal:18280'],
				['external_secret', 'pgw_dev_DevelopmentOnlyKey0123456789'],
				['external_model', 'openai/gpt-dev'],
				['acknowledge_cost', 'on'],
				...BASE.map(([k, v]): [string, string] => (k === 'max_calls' ? [k, '5000'] : [k, v]))
			])
		);
		expect(r.error).toBeNull();
		const req = r.request as unknown as Built;
		expect(req.external.base_url).toBe('http://host.docker.internal:18280');
		expect(req.external.secret).toBe('pgw_dev_DevelopmentOnlyKey0123456789');
		expect(req.load.max_calls).toBe(5000);
		expect(req.acknowledge_cost).toBe(true);
	});

	it('says what is missing in a sentence', () => {
		const missing = (fields: [string, string][]) => loadRequestFrom(form(fields)).error;
		expect(missing([['target', 'live'], ...BASE])).toBe('Choose the model.');
		expect(missing([['target', 'external'], ...BASE])).toBe('Paste the key the calls carry.');
		expect(
			missing([
				['target', 'external'],
				['external_secret', 'k'],
				['external_model', 'm'],
				['external_url', 'lite:8080'],
				...BASE
			])
		).toMatch(/URL/);
		expect(missing([['target', 'mock'], ...BASE.filter(([k]) => k !== 'agent')])).toMatch(/agent/);
		expect(
			missing([
				['target', 'mock'],
				...BASE.map(([k, v]): [string, string] => (k === 'timeout_s' ? [k, '0'] : [k, v]))
			])
		).toMatch(/timeout/i);
	});
});

describe('stages and figures', () => {
	it('adds up the stages and starts from a preset', () => {
		const ramp = preset('ramp', 100, 60);
		expect(totalDuration(ramp)).toBe(15 + 60 + 8);
		expect(peakOf(ramp)).toBe(100);
		expect(preset('steady', 5, 30)[0]).toEqual({ duration_s: 0, target: 5 });
		expect(peakOf(preset('steps', 40, 40))).toBe(40);
	});

	it('writes times and latencies for people', () => {
		expect(clock(65)).toBe('1:05');
		expect(clock(3725)).toBe('1:02:05');
		expect(ms(1.234)).toBe('1.23 ms');
		expect(ms(250)).toBe('250 ms');
		expect(ms(1500)).toBe('1.50 s');
		expect(ms(null)).toBe('–');
	});
});

describe('the charts', () => {
	const point = (t: number, extra: Partial<Point> = {}): Point => ({
		t,
		target: 10,
		started: 10,
		completed: 10,
		ok: 9,
		failed: 1,
		cancelled: 0,
		status: { '200': 9, '503': 1 },
		bytes: 100,
		in_flight: 10,
		skipped: 0,
		ttfb: { p50: 200, p95: 210, p99: 215, max: 220 },
		total: { p50: 580, p95: 590, p99: 600, max: 610 },
		gw_delay: null,
		cpu_max: 30,
		lag_max_ms: 0.2,
		gw_in_flight: 11,
		agents: 2,
		...extra
	});

	it('draws one series a measure, and leaves out what an external target lacks', () => {
		const points = [point(0), point(1, { ok: 10, failed: 0 })];
		const own = chartsOf(points, 'mock', 'concurrency');
		expect(own.map((c) => c.title)).toContain("The gateway's own delay");
		const calls = own[0];
		expect(calls.labels).toEqual(['0:00', '0:01']);
		expect(calls.series.find((s) => s.name === 'Successful')?.values).toEqual([9, 10]);
		const inFlight = own.find((c) => c.title === 'Calls in flight')!;
		expect(inFlight.series.map((s) => s.name)).toEqual([
			'At the agents',
			'At the gateway',
			'Target'
		]);
		const external = chartsOf(points, 'external', 'rate').find(
			(c) => c.title === 'Calls in flight'
		)!;
		expect(external.series.map((s) => s.name)).toEqual(['At the agents']);
		const delay = own.find((c) => c.title === "The gateway's own delay")!;
		expect(delay.series[0].values).toEqual([null, null]);
	});

	it('adds up the seconds while the run goes', () => {
		const t = runningTotals([
			point(0),
			point(1, { completed: 12, ok: 12, failed: 0, cpu_max: 70 })
		]);
		expect(t).toMatchObject({ calls: 22, ok: 21, failed: 1, rate: 12, cpu: 70 });
	});
});
