import { describe, expect, it } from 'vitest';
import { rateList, rateText, runSummary } from './prices';

describe('rateList', () => {
	it('lists the rates a price bills, in a fixed order', () => {
		const price = {
			output: 10,
			input: 2,
			cache_read: null,
			per_request: 0.01,
			variants: { batch: {} }
		};
		expect(rateList(price)).toEqual([
			{ label: 'Input', value: '2.00' },
			{ label: 'Output', value: '10.00' },
			{ label: 'Also', value: '0.01 per request' },
			{ label: 'Variants', value: 'batch' }
		]);
		expect(rateText({ input: 2.5 })).toBe('Input 2.50');
		expect(rateList({})).toEqual([]);
	});
});

describe('runSummary', () => {
	it('says what a check, a scan and an import came to', () => {
		expect(
			runSummary({
				pages: [{ outcome: 'read' }, { outcome: 'unchanged' }, { outcome: 'read' }],
				models: [{ outcome: 'applied' }, { outcome: 'no_price' }]
			})
		).toBe('pages: 2 read, 1 unchanged; models: 1 applied, 1 no price');
		expect(runSummary({ providers: [{ proposed: ['a', 'b'] }, { proposed: [] }] })).toBe(
			'2 providers scanned, 2 pages proposed'
		);
		expect(runSummary({ counts: { applied: 3, pinned: 1 } })).toBe('3 applied, 1 pinned');
		expect(runSummary(null)).toBe('');
	});
});
