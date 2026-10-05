import { describe, expect, it } from 'vitest';
import { formatValue } from '../report/format';
import { cellText, type Column } from './table';

const column = (kind?: Column<unknown>['kind']): Column<unknown> => ({
	key: 'x',
	label: 'X',
	kind
});

describe('cellText', () => {
	it('writes a number as an integer with separators', () => {
		expect(cellText(column('number'), 10440)).toBe('10,440');
		expect(cellText(column('number'), 0)).toBe('0');
	});

	it('shows the text a number column computed itself', () => {
		// Report tables format by the field's name and still set the column to the right.
		expect(cellText(column('number'), formatValue('requests', 10440))).toBe('10,440');
		expect(cellText(column('number'), formatValue('gateway_delay_ms', 0.049542))).toBe('0.05 ms');
		expect(cellText(column('number'), formatValue('success_ratio', 1))).toBe('100%');
	});

	it('is empty for a missing value, whatever the kind', () => {
		expect(cellText(column('number'), null)).toBe('');
		expect(cellText(column('number'), '')).toBe('');
		expect(cellText(column(), undefined)).toBe('');
		expect(cellText(column('list'), [])).toBe('');
	});

	it('writes lists and booleans', () => {
		expect(cellText(column('list'), ['a', 'b'])).toBe('a, b');
		expect(cellText(column('bool'), true)).toBe('Yes');
		expect(cellText(column('bool'), undefined)).toBe('No');
	});
});
