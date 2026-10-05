<script lang="ts" generics="T">
	import type { Snippet } from 'svelte';
	import Badge from './Badge.svelte';
	import { cellText, cellValue, type Column } from './table';

	/**
	 * A table from a column descriptor. It scrolls sideways inside its own container, so a
	 * wide table never widens the page.
	 */
	interface Props {
		columns: Column<T>[];
		rows: T[];
		/** Names the table for screen readers. */
		caption: string;
		/** A stable key per row. Default: the row's `id`. */
		rowKey?: (row: T) => string | number;
		/** Buttons and links at the end of each row. */
		actions?: Snippet<[T]>;
		/** Renders the cells of the columns named in `custom`. */
		cell?: Snippet<[T, Column<T>]>;
		custom?: string[];
	}

	let { columns, rows, caption, rowKey, actions, cell, custom = [] }: Props = $props();

	function keyOf(row: T, index: number): string | number {
		if (rowKey) return rowKey(row);
		const id = (row as { id?: unknown }).id;
		return typeof id === 'string' || typeof id === 'number' ? id : index;
	}
</script>

<div class="table-wrap">
	<table>
		<caption class="visually-hidden">{caption}</caption>
		<thead>
			<tr>
				{#each columns as column (column.key)}
					<th scope="col" class:num={column.kind === 'number'}>{column.label}</th>
				{/each}
				{#if actions}<th scope="col" class="actions"
						><span class="visually-hidden">Actions</span></th
					>{/if}
			</tr>
		</thead>
		<tbody>
			{#each rows as row, index (keyOf(row, index))}
				<tr>
					{#each columns as column (column.key)}
						{@const value = cellValue(row, column)}
						{@const href = column.href?.(row)}
						<td class:num={column.kind === 'number'} class:wrap={column.wrap}>
							{#if cell && custom.includes(column.key)}
								{@render cell(row, column)}
							{:else if column.kind === 'status'}
								{#if typeof value === 'string' && value}<Badge {value} />{/if}
							{:else}
								{@const text = cellText(column, value)}
								{#if text === ''}
									<span class="muted">{column.empty ?? '–'}</span>
								{:else if href}
									<a {href} class:mono={column.kind === 'code'}>{text}</a>
								{:else}
									<span
										class:mono={column.kind === 'code'}
										class:nowrap={column.kind === 'datetime'}>{text}</span
									>
								{/if}
							{/if}
						</td>
					{/each}
					{#if actions}
						<td class="actions"><div class="row-actions">{@render actions(row)}</div></td>
					{/if}
				</tr>
			{/each}
		</tbody>
	</table>
</div>

<style>
	/* Cells stay on one line and the table scrolls inside its container. */
	td {
		white-space: nowrap;
	}

	td.wrap {
		min-width: 16rem;
		white-space: normal;
		overflow-wrap: break-word;
	}

	.actions {
		width: 1%;
	}

	.row-actions {
		display: flex;
		justify-content: flex-end;
		gap: var(--space-1);
	}

	/* On a wide screen the actions stay in view while the columns scroll under them. */
	@media (min-width: 52rem) {
		.actions {
			position: sticky;
			right: 0;
		}

		td.actions {
			background: var(--surface);
			box-shadow: -1px 0 0 var(--border);
		}

		th.actions {
			box-shadow: -1px 0 0 var(--border);
		}
	}
</style>
