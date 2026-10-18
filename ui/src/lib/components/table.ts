import { formatDateTime, formatInteger } from '../format';

/** One column of a `DataTable`. */
export interface Column<T> {
	/** The property of the row that is shown, or any name for a computed column. */
	key: string;
	label: string;
	/**
	 * How the value is shown. Default `text`. A `number` column is set to the right; a number
	 * is written as an integer, and text a column computed itself (a cost, a duration) is
	 * shown as it is.
	 */
	kind?: 'text' | 'code' | 'status' | 'datetime' | 'list' | 'bool' | 'number';
	/** Computes the value when it is not simply `row[key]`. */
	value?: (row: T) => unknown;
	/** Makes the cell a link. */
	href?: (row: T) => string | null;
	/** Shown for a missing value or an empty list. */
	empty?: string;
	/** Long text: give it room to wrap over several lines. */
	wrap?: boolean;
	/**
	 * The value rows are sorted by. Default: the raw value when it is a number, a date or a
	 * boolean, otherwise the cell's text. Give it when `value` formats a number as text.
	 */
	sortValue?: (row: T) => string | number | null | undefined;
	/** Whether the column can be sorted. Default: when it has a label. */
	sortable?: boolean;
}

export type SortDirection = 'asc' | 'desc';

const COLLATOR = new Intl.Collator('en', { numeric: true, sensitivity: 'base' });

/** Whether a column offers sorting. */
export function isSortable<T>(column: Column<T>): boolean {
	return column.sortable ?? column.label.trim() !== '';
}

/** What a row is sorted by in a column; `null` for a missing value. */
export function sortKey<T>(row: T, column: Column<T>): string | number | null {
	if (column.sortValue) {
		const v = column.sortValue(row);
		return v === undefined || v === null || v === '' ? null : v;
	}
	const raw = cellValue(row, column);
	if (typeof raw === 'number') return Number.isFinite(raw) ? raw : null;
	if (typeof raw === 'boolean') return raw ? 1 : 0;
	if (column.kind === 'datetime' && typeof raw === 'string') {
		const time = Date.parse(raw);
		return Number.isNaN(time) ? null : time;
	}
	const text = cellText(column, raw);
	if (text === '') return null;
	// A number column whose value is text it wrote itself: "1,234", "0.05 ms", "12%".
	if (column.kind === 'number') {
		const number = Number.parseFloat(text.replace(/[^\d.eE+-]/g, ''));
		if (Number.isFinite(number)) return number;
	}
	return text;
}

/** Orders two rows by a column. Missing values come last in either direction. */
export function compareRows<T>(a: T, b: T, column: Column<T>, direction: SortDirection): number {
	const x = sortKey(a, column);
	const y = sortKey(b, column);
	if (x === null || y === null) return x === y ? 0 : x === null ? 1 : -1;
	const order =
		typeof x === 'number' && typeof y === 'number' ? x - y : COLLATOR.compare(String(x), String(y));
	return direction === 'asc' ? order : -order;
}

/** The rows in a column's order; rows that compare equal keep their order. */
export function sortRows<T>(rows: T[], column: Column<T>, direction: SortDirection): T[] {
	return rows
		.map((row, index) => ({ row, index }))
		.sort((a, b) => compareRows(a.row, b.row, column, direction) || a.index - b.index)
		.map(({ row }) => row);
}

/** Whether a row shows every word of the query, in any of its columns. */
export function matchesFilter<T>(row: T, columns: Column<T>[], query: string): boolean {
	const words = query.toLowerCase().split(/\s+/).filter(Boolean);
	if (words.length === 0) return true;
	const text = columns
		.map((column) => cellText(column, cellValue(row, column)))
		.join(' ')
		.toLowerCase();
	return words.every((word) => text.includes(word));
}

/**
 * The columns to show for these rows: a column named in `optional` only when some row has a
 * value in it, so that a column of dashes does not take room from the others.
 */
export function filledColumns<T>(columns: Column<T>[], rows: T[], optional: string[]): Column<T>[] {
	return columns.filter(
		(column) =>
			!optional.includes(column.key) ||
			rows.some((row) => cellText(column, cellValue(row, column)) !== '')
	);
}

export function cellValue<T>(row: T, column: Column<T>): unknown {
	if (column.value) return column.value(row);
	return (row as Record<string, unknown>)[column.key];
}

/** The text of a cell. Empty when there is nothing to show; the table then shows the column's `empty`. */
export function cellText<T>(column: Column<T>, value: unknown): string {
	switch (column.kind) {
		case 'datetime':
			return formatDateTime(value);
		case 'list':
			return Array.isArray(value) ? value.map(String).join(', ') : '';
		case 'bool':
			return value === true ? 'Yes' : 'No';
		case 'number':
			// A column that computes its own text (a cost, a duration) has written the number already.
			return typeof value === 'string' ? value : formatInteger(value);
		default:
			return value === undefined || value === null ? '' : String(value);
	}
}
