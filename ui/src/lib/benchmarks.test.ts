import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import {
	alignReports,
	cellRows,
	deltaMs,
	factRows,
	latencyBars,
	ms,
	overheadBars,
	type BenchmarkReport
} from './benchmarks.ts';

// The contract's own fixtures (crates/pacod-gw-bench-report/tests/fixtures/v1).
const fixture = (name: string): BenchmarkReport =>
	JSON.parse(
		readFileSync(
			new URL(`../../../crates/pacod-gw-bench-report/tests/fixtures/v1/${name}`, import.meta.url),
			'utf8'
		)
	);

describe('benchmark reports', () => {
	const full = fixture('full.json');

	it('measures each target cell against the baseline it names', () => {
		const rows = cellRows(full);
		expect(rows[0].overheadP50).toBeNull();
		expect(rows[1].overheadP50).toBeCloseTo(125.4 - 103.9, 5);
		expect(rows[2].overheadP99).toBeCloseTo(128.5 - 170.6, 5);
	});

	it('leaves the gateway figure of a baseline absent, and writes it n/a', () => {
		const bars = latencyBars(full, 'gw_delay');
		expect(bars.labels).toEqual(['mode managed', 'mode proxy']);
		expect(bars.series[2].values).toEqual([0.199, 0.126]);
		expect(ms(null)).toBe('n/a');
		expect(deltaMs(-2.5)).toBe('−2.50 ms');
		expect(overheadBars(full).series[0].values.every((v) => v !== null)).toBe(true);
	});

	it('lists facts with where they come from, and marks differences', () => {
		const other = fixture('external-third-party.json');
		const facts = factRows([full, other]);
		expect(facts[0]).toMatchObject({
			name: 'gateway_version',
			values: ['0.1.0', '–'],
			differs: true
		});
		const arch = facts.find((f) => f.name === 'arch');
		expect(arch).toMatchObject({
			values: ['arm64', 'arm64'],
			sources: ['declared', 'declared'],
			differs: false
		});
	});

	it('puts only cells of one configuration side by side', () => {
		const other = fixture('external-third-party.json');
		// The external report's cell has the managed cell's key: they ran the same configuration.
		const rows = alignReports([full, other]);
		expect(rows.map((r) => r.label)).toEqual(['mode managed']);
		expect(rows[0].cells[1]?.steady?.rps_mean).toBe(80);
		expect(alignReports([full])).toEqual([]);
	});
});
