/**
 * The response cache's report (`/analytics/cache`): what it did with the calls, the share it
 * answered and what its answers saved, as headline figures and over time.
 */
import { formatCost, formatInteger } from '../format';
import { numberOf, rowsOf, type Row } from './format';
import type { MeasureLine } from './stack';

const PERCENT = new Intl.NumberFormat('en-GB', { style: 'percent', maximumFractionDigits: 1 });

/** What the cache did with the calls, stacked: hits first, as they matter most. */
export const CACHE_RESULTS: MeasureLine[] = [
	{ field: 'hits', name: 'Answered from the cache', colour: '--series-1', bar: true },
	{ field: 'would_hits', name: 'Would have been (shadow)', colour: '--series-2', bar: true },
	{ field: 'misses', name: 'Not found', colour: '--series-3', bar: true },
	{ field: 'refreshes', name: 'Refreshed', colour: '--series-4', bar: true },
	{ field: 'bypasses', name: 'Not cacheable', colour: '--series-5', bar: true }
];

export const SAVED: MeasureLine[] = [{ field: 'saved_cost', name: 'Saved', colour: '--series-1' }];

/** The share of looked-up calls the cache answered; empty without lookups. */
export function hitRateText(totals: Row): string {
	const rate = totals.hit_rate;
	return typeof rate === 'number' ? PERCENT.format(rate) : '';
}

/**
 * What the hits saved. In the reporting currency, unless some hits had no exchange rate: then
 * each currency on its own.
 */
export function savedText(totals: Row, reportingCurrency: string): string {
	const currencies = rowsOf(totals, 'currencies');
	if (currencies.length > 1) {
		return currencies
			.map((c) => formatCost(numberOf(c, 'saved_cost'), String(c.currency ?? '')))
			.join(' + ');
	}
	const currency =
		currencies.length === 1 ? String(currencies[0].currency ?? '') : reportingCurrency;
	return formatCost(numberOf(totals, 'saved_cost'), currency) || '0.00';
}

/** The headline figures of the report. */
export function cacheFigures(totals: Row, reportingCurrency: string) {
	const lookups = numberOf(totals, 'lookups');
	const wouldHits = numberOf(totals, 'would_hits');
	return [
		{
			label: 'Hit rate',
			value: hitRateText(totals),
			note: lookups ? `Of ${formatInteger(lookups)} calls looked up` : 'No call was looked up'
		},
		{
			label: 'Answered from the cache',
			value: formatInteger(numberOf(totals, 'hits')),
			note: wouldHits
				? `${formatInteger(wouldHits)} more in shadow mode`
				: 'Calls the providers did not answer'
		},
		{
			label: 'Saved',
			value: savedText(totals, reportingCurrency),
			note: 'What the providers would have cost'
		},
		{
			label: 'Not cacheable',
			value: formatInteger(numberOf(totals, 'bypasses')),
			note: `${formatInteger(numberOf(totals, 'refreshes'))} refreshed at the caller’s request`
		}
	];
}

/** The rows of the lists by model and by workspace with their hit rate. */
export function withHitRate(rows: Row[]): Row[] {
	return rows.map((row) => {
		const lookups = numberOf(row, 'lookups');
		return { ...row, hit_rate: lookups > 0 ? numberOf(row, 'hits') / lookups : null };
	});
}
