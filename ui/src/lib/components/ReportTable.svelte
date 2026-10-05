<script lang="ts">
	import { formatValue, isNumeric, labelOf, type Row } from '#lib/report/format.ts';
	import DataTable from './DataTable.svelte';
	import type { Column } from './table.ts';

	/** The rows of a report as a table: one column per field, written by the field's name. */
	interface Props {
		caption: string;
		rows: Row[];
		/** The fields to show, in order. Default: those of the first row. */
		fields?: string[];
		/** Labels that differ from the field's usual one. */
		labels?: Record<string, string>;
		/** Makes the first column a link. */
		href?: (row: Row) => string | null;
		/** Shown when there are no rows. */
		empty: string;
		/** The field that identifies a row. */
		key?: string;
	}

	let { caption, rows, fields, labels = {}, href, empty, key }: Props = $props();

	const names = $derived(
		fields ??
			Object.keys(rows[0] ?? {}).filter(
				(name) => typeof rows[0][name] !== 'object' || rows[0][name] === null
			)
	);
	const columns = $derived(
		names.map((name, index): Column<Row> => ({
			key: name,
			label: labels[name] ?? labelOf(name),
			value: (row) => formatValue(name, row[name]),
			...(isNumeric(rows, name) ? { kind: 'number' as const } : {}),
			// A group without a name holds the calls that have no value for the grouping.
			...(name === 'group' ? { empty: 'Without a value' } : {}),
			...(index === 0 && href ? { href } : {})
		}))
	);
</script>

{#if rows.length === 0}
	<div class="card muted">{empty}</div>
{:else}
	<DataTable
		{caption}
		{columns}
		{rows}
		rowKey={(row) =>
			key && (typeof row[key] === 'string' || typeof row[key] === 'number')
				? row[key]
				: JSON.stringify(row)}
	/>
{/if}
