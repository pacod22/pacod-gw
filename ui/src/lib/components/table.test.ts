import { describe, expect, it } from 'vitest';
import { formatValue } from '../report/format';
import { cellText, compareRows, isSortable, matchesFilter, sortRows, type Column } from './table';

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

type Row = Record<string, unknown>;
const col = (key: string, more: Partial<Column<Row>> = {}): Column<Row> => ({
	key,
	label: key,
	...more
});
const keys = (rows: Row[]) => rows.map((r) => r.id);

describe('sortRows', () => {
	const rows: Row[] = [
		{ id: 'a', name: 'model-10', calls: 5, at: '2026-10-05T10:00:00Z', cost: '13.50' },
		{ id: 'b', name: 'model-9', calls: 40, at: '2026-10-04T10:00:00Z', cost: '2.00' },
		{ id: 'c', name: null, calls: null, at: null, cost: '' },
		{ id: 'd', name: 'Model-1', calls: 5, at: '2026-10-06T10:00:00Z', cost: '100.00' }
	];

	it('sorts text naturally and without regard to case', () => {
		expect(keys(sortRows(rows, col('name'), 'asc'))).toEqual(['d', 'b', 'a', 'c']);
	});

	it('sorts numbers and dates by value, and keeps equal rows in their order', () => {
		expect(keys(sortRows(rows, col('calls'), 'desc'))).toEqual(['b', 'a', 'd', 'c']);
		expect(keys(sortRows(rows, col('at', { kind: 'datetime' }), 'asc'))).toEqual([
			'b',
			'a',
			'd',
			'c'
		]);
	});

	it('reads the number in text a number column wrote itself', () => {
		expect(keys(sortRows(rows, col('cost', { kind: 'number' }), 'asc'))).toEqual([
			'b',
			'a',
			'd',
			'c'
		]);
	});

	it('puts missing values last in either direction', () => {
		expect(keys(sortRows(rows, col('calls'), 'asc')).at(-1)).toBe('c');
		expect(keys(sortRows(rows, col('calls'), 'desc')).at(-1)).toBe('c');
	});

	it('prefers an explicit sort value', () => {
		const byLength = col('name', { sortValue: (r) => String(r.name ?? '').length || null });
		expect(compareRows(rows[0], rows[1], byLength, 'asc')).toBeGreaterThan(0);
	});

	it('does not offer sorting on a column without a label', () => {
		expect(isSortable({ key: 'x', label: '' })).toBe(false);
		expect(isSortable({ key: 'x', label: 'X' })).toBe(true);
		expect(isSortable({ key: 'x', label: 'X', sortable: false })).toBe(false);
	});
});

describe('matchesFilter', () => {
	const columns = [col('name'), col('state', { kind: 'status' }), col('at', { kind: 'datetime' })];
	const row: Row = { name: 'openai/gpt-4.1', state: 'active', at: '2026-10-05T10:00:00Z' };

	it('needs every word, in any column, ignoring case', () => {
		expect(matchesFilter(row, columns, 'GPT active')).toBe(true);
		expect(matchesFilter(row, columns, 'gpt retired')).toBe(false);
		expect(matchesFilter(row, columns, '2026-10-05')).toBe(true);
		expect(matchesFilter(row, columns, '   ')).toBe(true);
	});
});
