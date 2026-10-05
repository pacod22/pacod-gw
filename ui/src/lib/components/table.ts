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
	/** Long text: let it wrap instead of keeping the cell on one line. */
	wrap?: boolean;
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
