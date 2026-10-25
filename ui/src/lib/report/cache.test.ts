import { describe, expect, it } from 'vitest';
import { cacheFigures, hitRateText, savedText, withHitRate } from './cache.ts';

const totals = {
	hits: 3,
	misses: 1,
	would_hits: 1,
	refreshes: 1,
	bypasses: 2,
	lookups: 5,
	saved_cost: 3.8,
	hit_rate: 0.6,
	currencies: [{ currency: 'USD', saved_cost: 3.8 }]
};

describe('the response cache report', () => {
	it('gives the hit rate, or nothing without lookups', () => {
		expect(hitRateText(totals)).toBe('60%');
		expect(hitRateText({ ...totals, hit_rate: null })).toBe('');
	});

	it('writes what was saved in one currency, or in each', () => {
		expect(savedText(totals, 'EUR')).toBe(savedText({ saved_cost: 3.8, currencies: [] }, 'USD'));
		const two = {
			...totals,
			currencies: [
				{ currency: 'USD', saved_cost: 3.3 },
				{ currency: 'EUR', saved_cost: 0.5 }
			]
		};
		expect(savedText(two, 'USD')).toMatch(/3\.30.* \+ .*0\.50/);
	});

	it('sums the report up in four figures', () => {
		const figures = cacheFigures(totals, 'USD');
		expect(figures.map((f) => f.label)).toEqual([
			'Hit rate',
			'Answered from the cache',
			'Saved',
			'Not cacheable'
		]);
		expect(figures[0].note).toBe('Of 5 calls looked up');
		expect(figures[1]).toMatchObject({ value: '3', note: '1 more in shadow mode' });
		expect(cacheFigures({ lookups: 0 }, 'USD')[0]).toMatchObject({
			value: '',
			note: 'No call was looked up'
		});
	});

	it('adds each row’s hit rate', () => {
		expect(
			withHitRate([
				{ hits: 1, lookups: 4 },
				{ hits: 0, lookups: 0 }
			])
		).toEqual([
			{ hits: 1, lookups: 4, hit_rate: 0.25 },
			{ hits: 0, lookups: 0, hit_rate: null }
		]);
	});
});
