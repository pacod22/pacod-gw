import { describe, expect, it } from 'vitest';
import { rateList, rateText } from './prices';

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
