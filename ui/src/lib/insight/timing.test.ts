import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { STAGES, timingOf } from './timing';
import { isUsageEvent, parseRequestPage } from './usage';

const fixture: unknown = JSON.parse(
	readFileSync(new URL('../fixtures/requests.json', import.meta.url), 'utf8')
);

describe('parseRequestPage', () => {
	it('reads the page shape of the analytics service', () => {
		const page = parseRequestPage(fixture);
		expect(page.requests).toHaveLength(4);
		expect(page.from).toBe('2026-10-03T12:00:00Z');
		expect(page.nextCursor).toMatch(/^\d+:/);
		expect(page.requests.every(isUsageEvent)).toBe(true);
	});

	it('copes with an answer that is not a page', () => {
		expect(parseRequestPage(null)).toEqual({
			from: null,
			to: null,
			requests: [],
			nextCursor: null
		});
		expect(parseRequestPage({ requests: [{ id: 1 }, 'x'], next_cursor: null }).requests).toEqual(
			[]
		);
	});
});

describe('timingOf', () => {
	const [streamed, retried, , rejected] = parseRequestPage(fixture).requests;

	it('lists the stages in the order they happen', () => {
		expect(timingOf(streamed).stages.map((s) => s.key)).toEqual(STAGES.map((s) => s.key));
		expect(STAGES.map((s) => s.owner)).toEqual([
			'client',
			'gateway',
			'connection',
			'connection',
			'provider',
			'provider',
			'gateway'
		]);
	});

	it('takes the gateway delay from the event and relates it to the total', () => {
		const timing = timingOf(streamed);
		expect(timing.gatewayDelay).toBeCloseTo(1.84);
		expect(timing.total).toBeCloseTo(2725.2);
		expect(timing.gatewayShare).toBeCloseTo(1.84 / 2725.2);
		expect(timing.sum).toBeCloseTo(0.62 + 1.31 + 412.5 + 2310 + 0.53);
		const shares = timing.stages.reduce((acc, s) => acc + s.share, 0);
		expect(shares).toBeCloseTo(1);
	});

	it('shows retries and connection time apart from the gateway delay', () => {
		const timing = timingOf(retried);
		const retry = timing.stages.find((s) => s.key === 'retry_ms');
		expect(retry?.ms).toBe(640);
		expect(retry?.owner).toBe('connection');
		expect(timing.gatewayDelay).toBeCloseTo(2.9);
	});

	it('handles a call that never reached a provider', () => {
		const timing = timingOf(rejected);
		expect(timing.stages.filter((s) => s.ms > 0).map((s) => s.key)).toEqual([
			'receive_ms',
			'pre_ms'
		]);
		expect(timing.gatewayDelay).toBeCloseTo(0.7);
	});

	it('treats missing and negative values as zero and derives the delay if absent', () => {
		const timing = timingOf({ pre_ms: 2, post_ms: 1, connect_ms: -5, total_ms: 0 });
		expect(timing.gatewayDelay).toBe(3);
		expect(timing.sum).toBe(3);
		expect(timing.gatewayShare).toBeNull();
		expect(timingOf({}).stages.every((s) => s.share === 0)).toBe(true);
	});
});
