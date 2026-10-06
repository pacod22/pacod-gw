/**
 * The analytics view: a range chosen from a few presets, headline figures over it, and the
 * calls, failures and tokens over time. As in PACOD, a day or less is shown per hour and a
 * longer range per day.
 */
import { formatCost, formatInteger } from '../format';
import { numberOf, rowsOf, type Row } from './format';
import type { MeasureLine } from './stack';

export interface Range {
	id: string;
	label: string;
	hours: number;
}

export const RANGES: Range[] = [
	{ id: '24h', label: 'Last 24 hours', hours: 24 },
	{ id: '7d', label: 'Last 7 days', hours: 7 * 24 },
	{ id: '30d', label: 'Last 30 days', hours: 30 * 24 },
	{ id: '90d', label: 'Last 90 days', hours: 90 * 24 },
	{ id: '180d', label: 'Last 180 days', hours: 180 * 24 }
];

/** The range of a preset's id; an unknown id is the fallback. */
export function rangeOf(id: string | null | undefined, fallback = '7d'): Range {
	return RANGES.find((r) => r.id === id) ?? RANGES.find((r) => r.id === fallback) ?? RANGES[1];
}

/** What a report is asked with for a range ending now: where it starts, and the bucket. */
export function rangeQuery(range: Range, now: number): { from: string; bucket: string } {
	// Whole minutes, so the start is the same for every report of one page.
	const from = Math.floor((now - range.hours * 3_600_000) / 60_000) * 60_000;
	return {
		from: new Date(from).toISOString().replace('.000Z', 'Z'),
		bucket: range.hours <= 24 ? 'hour' : 'day'
	};
}

const COUNTERS = [
	'requests',
	'errors',
	'input_tokens',
	'output_tokens',
	'cache_read_tokens',
	'cost'
] as const;

/** The counters of a usage report summed over every point of every series. */
export function totalsOf(data: unknown): Row {
	const totals: Row = Object.fromEntries(COUNTERS.map((key) => [key, 0]));
	for (const series of rowsOf(data, 'series'))
		for (const point of rowsOf(series, 'points'))
			for (const key of COUNTERS) totals[key] = numberOf(totals, key) + numberOf(point, key);
	return totals;
}

const COMPACT = new Intl.NumberFormat('en-GB', { notation: 'compact', maximumFractionDigits: 1 });
const PERCENT = new Intl.NumberFormat('en-GB', { style: 'percent', maximumFractionDigits: 1 });

/** `12.4k`, `3.1M`. */
export function formatCompact(value: number): string {
	return COMPACT.format(value);
}

export interface Figure {
	label: string;
	value: string;
	note?: string;
}

/**
 * The headline figures of a range: calls, tokens, cost and failures. `errorTypes` are the
 * failures per kind (`by_type` of the errors report), named under the failures.
 */
export function headlineFigures(totals: Row, errorTypes: Row[] = []): Figure[] {
	const requests = numberOf(totals, 'requests');
	const errors = numberOf(totals, 'errors');
	const input = numberOf(totals, 'input_tokens');
	const output = numberOf(totals, 'output_tokens');
	const kinds = errorTypes
		.filter((row) => numberOf(row, 'errors') > 0)
		.map(
			(row) => `${String(row.error_type || 'unknown')}: ${formatInteger(numberOf(row, 'errors'))}`
		);
	const rate = requests > 0 ? `${PERCENT.format(errors / requests)} of the calls` : '';
	return [
		{ label: 'Calls', value: formatInteger(requests) },
		{
			label: 'Tokens',
			value: formatCompact(input + output),
			note: `${formatCompact(input)} in · ${formatCompact(output)} out`
		},
		{ label: 'Cost', value: formatCost(numberOf(totals, 'cost'), '') || '0.00' },
		{
			label: 'Failed calls',
			value: formatInteger(errors),
			note: [rate, ...kinds].filter(Boolean).join(' · ') || undefined
		}
	];
}

/** The rows of a breakdown with the share of failed calls added. */
export function withErrorRate(rows: Row[]): Row[] {
	return rows.map((row) => {
		const requests = numberOf(row, 'requests');
		return { ...row, error_rate: requests > 0 ? numberOf(row, 'errors') / requests : 0 };
	});
}

/** Calls as a line and failed calls as columns, as PACOD shows them. */
export const CALLS_AND_FAILURES: MeasureLine[] = [
	{ field: 'requests', name: 'Calls', colour: '--series-1' },
	{ field: 'errors', name: 'Failed calls', colour: '--danger', bar: true }
];

export const TOKENS: MeasureLine[] = [
	{ field: 'input_tokens', name: 'Input tokens', colour: '--series-3' },
	{ field: 'output_tokens', name: 'Output tokens', colour: '--series-6' }
];

export const COST: MeasureLine[] = [{ field: 'cost', name: 'Cost', colour: '--series-5' }];

/**
 * The values a filter offers: the named field of the rows of a report (`group` of a breakdown,
 * `end_user` of the end users), without empty ones, in alphabetical order.
 */
export function choiceValues(data: unknown, field = 'group'): string[] {
	const values = rowsOf(data, 'rows')
		.map((row) => row[field])
		.filter((value): value is string => typeof value === 'string' && value !== '');
	return [...new Set(values)].sort((a, b) => a.localeCompare(b));
}
