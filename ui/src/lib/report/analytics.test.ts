import { describe, expect, it } from 'vitest';
import {
	choiceValues,
	headlineFigures,
	rangeOf,
	rangeQuery,
	totalsOf,
	withErrorRate
} from './analytics';

describe('the range of the analytics view', () => {
	it('takes a preset, falling back to a week or to the fallback given', () => {
		expect(rangeOf('30d').hours).toBe(720);
		expect(rangeOf('nonsense').id).toBe('7d');
		expect(rangeOf(null, '30d').id).toBe('30d');
	});

	it('starts at a whole minute and goes per hour up to a day, per day beyond', () => {
		const now = Date.parse('2026-10-05T19:06:42.500Z');
		expect(rangeQuery(rangeOf('24h'), now)).toEqual({
			from: '2026-10-04T19:06:00Z',
			bucket: 'hour'
		});
		expect(rangeQuery(rangeOf('7d'), now)).toEqual({
			from: '2026-09-28T19:06:00Z',
			bucket: 'day'
		});
	});
});

describe('the headline figures', () => {
	it('sums a usage report over its series and points', () => {
		const totals = totalsOf({
			series: [
				{ group: null, points: [{ requests: 2, errors: 1, cost: 0.5, input_tokens: 10 }] },
				{ group: null, points: [{ requests: 3, errors: 0, cost: 0.25, output_tokens: 7 }] }
			]
		});
		expect(totals).toMatchObject({ requests: 5, errors: 1, cost: 0.75, input_tokens: 10 });
	});

	it('writes calls, tokens, cost and failures with their rate and kinds', () => {
		const figures = headlineFigures(
			{ requests: 4, errors: 1, input_tokens: 1500, output_tokens: 500, cost: 1.5 },
			[
				{ error_type: 'rate_limited', errors: 1 },
				{ error_type: 'upstream_timeout', errors: 0 }
			]
		);
		expect(figures).toEqual([
			{ label: 'Calls', value: '4' },
			{ label: 'Tokens', value: '2k', note: '1.5k in · 500 out' },
			{ label: 'Cost', value: '1.50' },
			{ label: 'Failed calls', value: '1', note: '25% of the calls · rate_limited: 1' }
		]);
		expect(headlineFigures({}).at(-1)).toEqual({ label: 'Failed calls', value: '0' });
	});

	it('adds the failure rate to the rows of a breakdown', () => {
		expect(withErrorRate([{ requests: 4, errors: 1 }, { requests: 0 }])).toEqual([
			{ requests: 4, errors: 1, error_rate: 0.25 },
			{ requests: 0, error_rate: 0 }
		]);
	});
});

describe('the values a filter offers', () => {
	it('lists the named field of the rows once, without empty values, in order', () => {
		const breakdown = {
			rows: [{ group: 'gpt-5' }, { group: '' }, { group: 'claude' }, { group: null }]
		};
		expect(choiceValues(breakdown)).toEqual(['claude', 'gpt-5']);
		const users = { rows: [{ end_user: 'bob' }, { end_user: 'alice' }, { end_user: 'bob' }] };
		expect(choiceValues(users, 'end_user')).toEqual(['alice', 'bob']);
		expect(choiceValues(null)).toEqual([]);
	});
});
