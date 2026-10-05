/** One column of a `DataTable`. */
export interface Column<T> {
	/** The property of the row that is shown, or any name for a computed column. */
	key: string;
	label: string;
	/** How the value is shown. Default `text`. */
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
