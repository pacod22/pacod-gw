import { describe, expect, it } from 'vitest';
import {
	breakdownHref,
	measureSeries,
	bucketSearch,
	bucketSequence,
	bucketSpan,
	shareRows,
	stackSeries
} from './stack';

const iso = (ms: number) => new Date(ms).toISOString().replace('.000Z', 'Z');

describe('the time axis of a stacked chart', () => {
	it('lists every hour of a range, starting with the hour the range starts in', () => {
		const hours = bucketSequence('2026-10-05T07:37:12Z', '2026-10-05T10:30:00Z', 'hour');
		expect(hours?.map(iso)).toEqual([
			'2026-10-05T07:00:00Z',
			'2026-10-05T08:00:00Z',
			'2026-10-05T09:00:00Z',
			'2026-10-05T10:00:00Z'
		]);
	});

	it('steps by day, by week from Monday and by month in UTC', () => {
		expect(bucketSequence('2026-03-01T00:00:00Z', '2026-03-04T00:00:00Z', 'day')?.map(iso)).toEqual(
			['2026-03-01T00:00:00Z', '2026-03-02T00:00:00Z', '2026-03-03T00:00:00Z']
		);
		// 2026-10-07 is a Wednesday; its week starts on Monday the 5th.
		expect(
			bucketSequence('2026-10-07T12:00:00Z', '2026-10-20T00:00:00Z', 'week')?.map(iso)
		).toEqual(['2026-10-05T00:00:00Z', '2026-10-12T00:00:00Z', '2026-10-19T00:00:00Z']);
		expect(
			bucketSequence('2026-01-15T00:00:00Z', '2026-03-02T00:00:00Z', 'month')?.map(iso)
		).toEqual(['2026-01-01T00:00:00Z', '2026-02-01T00:00:00Z', '2026-03-01T00:00:00Z']);
	});

	it('gives up on an unknown or too long range', () => {
		expect(bucketSequence(undefined, '2026-03-04T00:00:00Z', 'day')).toBeNull();
		expect(bucketSequence('2026-03-04T00:00:00Z', '2026-03-01T00:00:00Z', 'day')).toBeNull();
		expect(bucketSequence('2020-01-01T00:00:00Z', '2026-01-01T00:00:00Z', 'hour')).toBeNull();
	});

	it('reaches back by the span that suits the bucket', () => {
		expect(bucketSpan('hour')).toBe(48 * 3_600_000);
		expect(bucketSpan('month')).toBe(365 * 86_400_000);
	});
});

const usage = (series: unknown[]) => ({
	from: '2026-03-01T00:00:00Z',
	to: '2026-03-04T00:00:00Z',
	bucket: 'day',
	group_by: 'model',
	series
});

describe('a usage report as stacked series', () => {
	it('fills the days without calls and matches points by instant', () => {
		const stack = stackSeries(
			usage([
				{
					group: 'gpt',
					points: [
						{ bucket: '2026-03-01T00:00:00Z', cost: 1, input_tokens: 10, output_tokens: 5 },
						{ bucket: '2026-03-03T00:00:00+00:00', cost: 2, input_tokens: 1, output_tokens: 1 }
					]
				}
			]),
			'cost'
		);
		expect(stack.labels).toEqual(['2026-03-01', '2026-03-02', '2026-03-03']);
		expect(stack.series).toEqual([{ name: 'gpt', values: [1, 0, 2], colour: '--series-1' }]);
		expect(stackSeries(usage(rowsFor('gpt')), 'tokens').series[0].values).toEqual([15, 0, 2]);
	});

	it('orders groups by cost whatever is measured, and folds the rest into Other', () => {
		const series = ['a', 'b', 'c', 'd'].map((group, i) => ({
			group,
			points: [{ bucket: '2026-03-02T00:00:00Z', cost: i + 1, requests: 10 - i }]
		}));
		const byCost = stackSeries(usage(series), 'cost', 2);
		const byRequests = stackSeries(usage(series), 'requests', 2);
		expect(byCost.series.map((s) => s.name)).toEqual(['d', 'c', 'Other']);
		expect(byCost.series.map((s) => s.colour)).toEqual([
			'--series-1',
			'--series-2',
			'--series-other'
		]);
		expect(byRequests.series.map((s) => s.name)).toEqual(['d', 'c', 'Other']);
		expect(byCost.series[2].values).toEqual([0, 3, 0]);
		expect(byRequests.series[2].values).toEqual([0, 19, 0]);
	});

	it('names a group without a value, and shows nothing without points', () => {
		const stack = stackSeries(
			usage([{ group: '', points: [{ bucket: '2026-03-01T00:00:00Z', requests: 1 }] }]),
			'requests'
		);
		expect(stack.series[0].name).toBe('(none)');
		expect(stackSeries(usage([]), 'cost')).toEqual({ labels: [], series: [] });
		expect(stackSeries(null, 'cost')).toEqual({ labels: [], series: [] });
	});
});

function rowsFor(group: string) {
	return [
		{
			group,
			points: [
				{ bucket: '2026-03-01T00:00:00Z', cost: 1, input_tokens: 10, output_tokens: 5 },
				{ bucket: '2026-03-03T00:00:00Z', cost: 2, input_tokens: 1, output_tokens: 1 }
			]
		}
	];
}

describe('shares of the cost', () => {
	it('takes the largest groups and folds the rest into Other', () => {
		const rows = [
			{ group: 'openai', cost: 6 },
			{ group: 'anthropic', cost: 3 },
			{ group: 'google', cost: 0.5 },
			{ group: '', cost: 0.5 }
		];
		expect(shareRows(rows, 2)).toEqual([
			{ name: 'openai', cost: 6, share: 0.6 },
			{ name: 'anthropic', cost: 3, share: 0.3 },
			{ name: 'Other', cost: 1, share: 0.1 }
		]);
		expect(shareRows([{ group: 'x', cost: 0 }])).toEqual([]);
	});
});

describe('links of the overview', () => {
	it('switches the bucket over the span that suits it, keeping the other filters', () => {
		expect(bucketSearch('?model=gpt&bucket=hour', '2026-10-05T10:30:00Z', 'day')).toBe(
			'?model=gpt&bucket=day&from=2026-09-05T10%3A30'
		);
		expect(bucketSearch('', undefined, 'week')).toBe('?bucket=week');
	});

	it('opens the breakdown with the filters but without the time series parameters', () => {
		expect(breakdownHref('?model=gpt&bucket=day&group_by=template', 'provider')).toBe(
			'/insight/breakdown?model=gpt&by=provider'
		);
	});
});

describe('measures over time', () => {
	it('sums the groups per bucket, fills the gaps and reads a list of rows', () => {
		const lines = [
			{ field: 'requests', name: 'Calls', colour: '--series-1' },
			{ field: 'errors', name: 'Failed calls', colour: '--danger', bar: true }
		];
		const grouped = usage([
			{ group: 'a', points: [{ bucket: '2026-03-01T00:00:00Z', requests: 2, errors: 1 }] },
			{ group: 'b', points: [{ bucket: '2026-03-01T00:00:00Z', requests: 3, errors: 0 }] }
		]);
		expect(measureSeries(grouped, lines)).toEqual({
			labels: ['2026-03-01', '2026-03-02', '2026-03-03'],
			series: [
				{ name: 'Calls', values: [5, 0, 0], colour: '--series-1' },
				{ name: 'Failed calls', values: [1, 0, 0], colour: '--danger', bar: true }
			]
		});
		const errors = { ...usage([]), over_time: [{ bucket: '2026-03-02T00:00:00Z', errors: 4 }] };
		expect(measureSeries(errors, lines, 'over_time').series[1].values).toEqual([0, 4, 0]);
		expect(measureSeries(usage([]), lines)).toEqual({ labels: [], series: [] });
	});
});
