import { describe, expect, it } from 'vitest';
import { BOARDS, expand, metricRangeOf, stepsOf } from './boards';
import {
	chartSeries,
	formatUnit,
	legendOf,
	statValue,
	tableRows,
	timeAxis,
	timeLabel
} from './series';

describe('the queries of the metrics boards', () => {
	it('matches everything without a filter, and a chosen value exactly', () => {
		const expr = 'rate(gw_requests_total{organisation=~"$organisation", model=~"$model"}[$rate])';
		expect(expand(expr, {}, 60, 3600)).toBe(
			'rate(gw_requests_total{organisation=~".*", model=~".*"}[60s])'
		);
		expect(expand(expr, { model: 'gpt-4.1' }, 60, 3600)).toBe(
			'rate(gw_requests_total{organisation=~".*", model=~"gpt-4\\\\.1"}[60s])'
		);
		expect(expand('increase(x{instance=~"$instance"}[$range])', { instance: 'a"b' }, 60, 600)).toBe(
			'increase(x{instance=~"a\\"b"}[600s])'
		);
	});

	it('leaves no placeholder behind on any board', () => {
		for (const board of BOARDS)
			for (const section of board.sections)
				for (const panel of section.panels)
					for (const query of panel.queries)
						expect(expand(query.expr, {}, 60, 3600), `${board.id}: ${panel.title}`).not.toMatch(
							/\$/
						);
	});

	it('steps about 120 times over a range, rates over at least four scrapes', () => {
		expect(stepsOf(metricRangeOf('1h').seconds)).toEqual({ step: 30, rate: 60 });
		expect(stepsOf(metricRangeOf('6h').seconds)).toEqual({ step: 180, rate: 195 });
		expect(metricRangeOf('nonsense').id).toBe('6h');
	});
});

describe('answers of Prometheus', () => {
	it('names a series by its legend or its labels', () => {
		expect(legendOf({ provider: 'openai', model: 'gpt' }, '{{provider}} / {{model}}')).toBe(
			'openai / gpt'
		);
		expect(legendOf({ provider: 'openai' }, '{{provider}} ({{currency}})')).toBe('openai (–)');
		expect(legendOf({ __name__: 'up', job: 'core' })).toBe('job=core');
		expect(legendOf({})).toBe('value');
	});

	it('puts series on the axis with gaps where nothing was measured', () => {
		const axis = timeAxis(100, 160, 30);
		expect(axis).toEqual([100, 130, 160]);
		const series = chartSeries(
			[
				{
					result: [
						{
							metric: { outcome: 'ok' },
							values: [
								[100, '1.5'],
								[160, 'NaN']
							]
						},
						{ metric: { outcome: 'error' }, values: [[130, '2']] }
					],
					legend: '{{outcome}}'
				}
			],
			axis
		);
		expect(series).toEqual([
			{ name: 'ok', values: [1.5, null, null], colour: '--series-1' },
			{ name: 'error', values: [null, 2, null], colour: '--series-2' }
		]);
		expect(timeLabel(1_791_234_000, 3600)).toBe('21:00');
		expect(timeLabel(1_791_234_000, 7 * 86_400)).toBe('10-05 21:00');
	});

	it('writes values in their unit', () => {
		expect(formatUnit(1.234, 'reqps')).toBe('1.23/s');
		expect(formatUnit(0.0456, 'percent')).toBe('4.6%');
		expect(formatUnit(0.0012, 'seconds')).toBe('1.20 ms');
		expect(formatUnit(2.5, 'seconds')).toBe('2.5 s');
		expect(formatUnit(3 * 1024 * 1024, 'bytes')).toBe('3 MiB');
		expect(formatUnit(12_400, 'short')).toBe('12.4k');
		expect(formatUnit(null, 'short')).toBe('–');
	});

	it('gives a headline figure per series, and table rows per label set', () => {
		expect(
			statValue(
				[
					{ metric: { currency: 'USD' }, value: [1, '1.5'] },
					{ metric: { currency: 'EUR' }, value: [1, '0.25'] }
				],
				'money',
				'{{currency}}'
			)
		).toBe('1.50 USD · 0.25 EUR');
		expect(statValue([], 'count')).toBe('–');
		expect(
			tableRows(
				[
					{ result: [{ metric: { workspace: 'a' }, value: [1, '10.4'] }], column: 'requests' },
					{ result: [{ metric: { workspace: 'a' }, value: [1, '99.6'] }], column: 'tokens' }
				],
				['workspace'],
				'short'
			)
		).toEqual([{ workspace: 'a', requests: 10, tokens: 100 }]);
	});
});
