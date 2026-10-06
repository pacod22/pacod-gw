<script lang="ts" generics="T">
	import type { Snippet } from 'svelte';
	import { onMount } from 'svelte';
	import ArrowDown from '@lucide/svelte/icons/arrow-down';
	import ArrowUp from '@lucide/svelte/icons/arrow-up';
	import ChevronsUpDown from '@lucide/svelte/icons/chevrons-up-down';
	import Search from '@lucide/svelte/icons/search';
	import Badge from './Badge.svelte';
	import RowMenu from './RowMenu.svelte';
	import {
		cellText,
		cellValue,
		isSortable,
		matchesFilter,
		sortRows,
		type Column,
		type SortDirection
	} from './table';

	/**
	 * A table from a column descriptor.
	 *
	 * It is made to fit: text wraps, identifiers are shortened with the full value on hover,
	 * and only dates, numbers and states keep to one line. A table that is still too wide
	 * scrolls inside its frame, with a shadow on the side where more is. On a narrow screen
	 * every row becomes a card of labelled values.
	 */
	interface Props {
		columns: Column<T>[];
		rows: T[];
		/** Names the table for screen readers. */
		caption: string;
		/** A stable key per row. Default: the row's `id`. */
		rowKey?: (row: T) => string | number;
		/** Buttons and links for each row. */
		actions?: Snippet<[T]>;
		/** Puts the actions behind one button per row, named by this, for example "Actions for x". */
		menu?: (row: T) => string;
		/** Renders the cells of the columns named in `custom`. */
		cell?: Snippet<[T, Column<T>]>;
		custom?: string[];
		/** Column headers sort the rows. Default on. */
		sortable?: boolean;
		/** Offers a box that filters the rows by their text. */
		filter?: boolean;
		/** The rows are one page of several: sorting and filtering apply to this page only. */
		paged?: boolean;
	}

	let {
		columns,
		rows,
		caption,
		rowKey,
		actions,
		menu,
		cell,
		custom = [],
		sortable = true,
		filter = false,
		paged = false
	}: Props = $props();

	let query = $state('');
	let sortKey: string | null = $state(null);
	let direction: SortDirection = $state('asc');

	const sortColumn = $derived(columns.find((c) => c.key === sortKey));
	const shown = $derived.by(() => {
		const kept =
			filter && query.trim() ? rows.filter((r) => matchesFilter(r, columns, query)) : rows;
		return sortColumn ? sortRows(kept, sortColumn, direction) : kept;
	});
	const width = $derived(columns.length + (actions ? 1 : 0));

	/** Text longer than this wraps; a column whose values are all shorter keeps to one line. */
	const LONG_TEXT = 32;
	const wrapping = $derived(
		new Set(
			columns
				.filter(
					(column) =>
						column.wrap ||
						((column.kind ?? 'text') === 'text' &&
							rows.some((row) => cellText(column, cellValue(row, column)).length > LONG_TEXT))
				)
				.map((column) => column.key)
		)
	);

	/** First click sorts up, the second down, the third restores the original order. */
	function toggleSort(column: Column<T>) {
		if (sortKey !== column.key) {
			sortKey = column.key;
			direction = 'asc';
		} else if (direction === 'asc') {
			direction = 'desc';
		} else {
			sortKey = null;
		}
	}

	/**
	 * A name made of parts, such as a model "provider/model": it may wrap after a slash and
	 * is never shortened. Plain identifiers are shortened instead.
	 */
	function isPath(column: Column<T>, text: string): boolean {
		return column.kind === 'code' && text.includes('/');
	}

	function keyOf(row: T, index: number): string | number {
		if (rowKey) return rowKey(row);
		const id = (row as { id?: unknown }).id;
		return typeof id === 'string' || typeof id === 'number' ? id : index;
	}

	// The side shadows: shown while the table extends beyond the frame on that side.
	let scroller: HTMLDivElement | undefined = $state();
	let moreLeft = $state(false);
	let moreRight = $state(false);

	function measure() {
		if (!scroller) return;
		const { scrollLeft, scrollWidth, clientWidth } = scroller;
		moreLeft = scrollLeft > 1;
		moreRight = scrollLeft + clientWidth < scrollWidth - 1;
	}

	onMount(() => {
		if (!scroller) return;
		const observer = new ResizeObserver(measure);
		observer.observe(scroller);
		const table = scroller.querySelector('table');
		if (table) observer.observe(table);
		measure();
		return () => observer.disconnect();
	});
</script>

{#snippet pathText(column: Column<T>, text: string)}
	{#if isPath(column, text)}
		{#each text.split('/') as part, i (i)}{#if i > 0}/<wbr />{/if}{part}{/each}
	{:else}
		{text}
	{/if}
{/snippet}

<div class="data-table">
	{#if filter}
		<div class="tools">
			<label class="search">
				<Search size={16} aria-hidden="true" />
				<span class="visually-hidden">Filter {caption}</span>
				<input type="search" placeholder="Filter" bind:value={query} autocomplete="off" />
			</label>
			<span class="muted small count" aria-live="polite">
				{#if query.trim()}{shown.length} of {rows.length}{:else}{rows.length}{/if}
				{rows.length === 1 ? 'row' : 'rows'}{paged ? ' on this page' : ''}
			</span>
		</div>
	{/if}

	<div class="frame" class:more-left={moreLeft} class:more-right={moreRight}>
		<div class="table-wrap" bind:this={scroller} onscroll={measure}>
			<!-- svelte-ignore a11y_no_redundant_roles (the card layout of narrow screens changes display, which drops the table semantics in Chromium and WebKit) -->
			<table role="table">
				<caption class="visually-hidden">
					{caption}{paged && sortKey ? ' (sorted within this page)' : ''}
				</caption>
				<!-- svelte-ignore a11y_no_redundant_roles (the card layout of narrow screens changes display, which drops the table semantics in Chromium and WebKit) -->
				<thead role="rowgroup">
					<!-- svelte-ignore a11y_no_redundant_roles -->
					<tr role="row">
						{#each columns as column (column.key)}
							{@const active = sortKey === column.key}
							<th
								role="columnheader"
								scope="col"
								class:num={column.kind === 'number'}
								aria-sort={active ? (direction === 'asc' ? 'ascending' : 'descending') : undefined}
							>
								{#if sortable && isSortable(column) && rows.length > 1}
									<button
										type="button"
										class="sort"
										class:active
										onclick={() => toggleSort(column)}
									>
										<!-- The arrow stays with the last word when the label wraps. -->
										{column.label}<span class="arrow"
											>{#if active && direction === 'asc'}<ArrowUp
													size={13}
													aria-hidden="true"
												/>{:else if active}<ArrowDown
													size={13}
													aria-hidden="true"
												/>{:else}<ChevronsUpDown
													size={13}
													aria-hidden="true"
													class="idle"
												/>{/if}</span
										>
									</button>
								{:else}
									{column.label}
								{/if}
							</th>
						{/each}
						{#if actions}
							<th role="columnheader" scope="col" class="actions">
								<span class="visually-hidden">Actions</span>
							</th>
						{/if}
					</tr>
				</thead>
				<!-- svelte-ignore a11y_no_redundant_roles (the card layout of narrow screens changes display, which drops the table semantics in Chromium and WebKit) -->
				<tbody role="rowgroup">
					{#each shown as row, index (keyOf(row, index))}
						<!-- svelte-ignore a11y_no_redundant_roles -->
						<tr role="row">
							{#each columns as column (column.key)}
								{@const value = cellValue(row, column)}
								{@const href = column.href?.(row)}
								<td
									role="cell"
									data-label={column.label}
									class="kind-{column.kind ?? 'text'}"
									class:num={column.kind === 'number'}
									class:wrap={wrapping.has(column.key)}
								>
									{#if cell && custom.includes(column.key)}
										{@render cell(row, column)}
									{:else if column.kind === 'status'}
										{#if typeof value === 'string' && value}<Badge {value} />{:else}<span
												class="muted">{column.empty ?? '–'}</span
											>{/if}
									{:else}
										{@const text = cellText(column, value)}
										{#if text === ''}
											<span class="muted">{column.empty ?? '–'}</span>
										{:else if href}
											<a
												{href}
												class="value"
												class:path={isPath(column, text)}
												title={column.kind === 'code' ? text : undefined}
												>{@render pathText(column, text)}</a
											>
										{:else}
											<span
												class="value"
												class:path={isPath(column, text)}
												title={column.kind === 'code' ? text : undefined}
												>{@render pathText(column, text)}</span
											>
										{/if}
									{/if}
								</td>
							{/each}
							{#if actions}
								<td role="cell" class="actions" class:with-menu={Boolean(menu)} data-label="">
									{#if menu}
										<RowMenu label={menu(row)}>{@render actions(row)}</RowMenu>
									{:else}
										<div class="row-actions">{@render actions(row)}</div>
									{/if}
								</td>
							{/if}
						</tr>
					{:else}
						<!-- svelte-ignore a11y_no_redundant_roles -->
						<tr role="row">
							<td role="cell" class="none" colspan={width}>No row matches “{query.trim()}”.</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	</div>
</div>

<style>
	.data-table {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		min-width: 0;
	}

	/* --- filter --- */

	.tools {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-2) var(--space-4);
	}

	.search {
		position: relative;
		display: flex;
		align-items: center;
		flex: 0 1 20rem;
		min-width: 12rem;
		color: var(--text-muted);
	}

	.search :global(svg) {
		position: absolute;
		left: 0.75rem;
		pointer-events: none;
	}

	.search input {
		padding-left: 2.25rem;
		background: var(--surface);
	}

	/* --- frame and side shadows --- */

	.frame {
		position: relative;
		min-width: 0;
	}

	.frame::before,
	.frame::after {
		content: '';
		position: absolute;
		top: 1px;
		bottom: 1px;
		z-index: 2;
		width: 1.5rem;
		opacity: 0;
		pointer-events: none;
		transition: opacity 0.15s;
	}

	.frame::before {
		left: 1px;
		border-radius: var(--radius-lg) 0 0 var(--radius-lg);
		background: linear-gradient(to right, rgb(0 0 0 / 0.08), transparent);
	}

	.frame::after {
		right: 1px;
		border-radius: 0 var(--radius-lg) var(--radius-lg) 0;
		background: linear-gradient(to left, rgb(0 0 0 / 0.08), transparent);
	}

	.frame.more-left::before,
	.frame.more-right::after {
		opacity: 1;
	}

	/* --- header --- */

	th {
		letter-spacing: 0.1em;
		white-space: normal;
		text-wrap: balance;
		vertical-align: bottom;
	}

	th.num {
		text-align: right;
	}

	.sort {
		display: inline;
		margin: -0.25rem -0.375rem;
		padding: 0.25rem 0.375rem;
		border: 0;
		border-radius: var(--radius-sm);
		background: none;
		color: inherit;
		font: inherit;
		letter-spacing: inherit;
		text-align: inherit;
		text-transform: inherit;
		cursor: pointer;
	}

	.arrow {
		display: inline-block;
		margin-left: 0.3rem;
		vertical-align: -0.15em;
		white-space: nowrap;
	}

	.sort:hover,
	.sort.active {
		color: var(--text);
	}

	.sort:hover {
		background: color-mix(in srgb, var(--border) 60%, transparent);
	}

	.sort :global(.idle) {
		opacity: 0.4;
	}

	.sort:hover :global(.idle) {
		opacity: 0.8;
	}

	/* --- cells --- */

	/* Short values keep to one line; columns with long text wrap (see `wrapping`). */
	td {
		white-space: nowrap;
	}

	/* "anywhere", not "break-word": only it lets a long URL narrow a table column. */
	td.wrap {
		min-width: 12rem;
		white-space: normal;
		overflow-wrap: anywhere;
	}

	/* One line: dates, numbers, yes/no and states. */
	/* Identifiers stay whole on one line and are shortened; the full value is in the title. */
	td.kind-code .value {
		display: inline-block;
		max-width: 11rem;
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: 0.75rem;
		text-overflow: ellipsis;
		vertical-align: bottom;
		white-space: nowrap;
	}

	td.kind-code .value.path {
		display: inline;
		max-width: none;
		white-space: normal;
	}

	td.kind-code:has(.path) {
		min-width: 12rem;
	}

	/* The first column names the row. */
	td:first-child .value {
		font-weight: 500;
	}

	td.kind-code:first-child .value {
		font-weight: 500;
	}

	td.none {
		padding: var(--space-5);
		color: var(--text-muted);
		text-align: center;
	}

	/* --- actions --- */

	.actions {
		width: 1%;
		text-align: right;
		white-space: nowrap;
	}

	td.actions {
		padding-top: 0;
		padding-bottom: 0;
	}

	.row-actions {
		display: flex;
		justify-content: flex-end;
		gap: var(--space-1);
	}

	.row-actions :global(form) {
		display: contents;
	}

	/* --- narrow screens: every row a card --- */

	@media (max-width: 48rem) {
		table,
		thead,
		tbody,
		tr,
		td {
			display: block;
		}

		thead {
			position: absolute;
			width: 1px;
			height: 1px;
			overflow: hidden;
			clip-path: inset(50%);
			white-space: nowrap;
		}

		.table-wrap {
			overflow: visible;
		}

		tbody tr {
			position: relative;
			padding: var(--space-3) var(--space-4);
			border-bottom: 1px solid var(--border);
		}

		tbody tr:last-child {
			border-bottom: 0;
		}

		tbody tr:hover > td {
			background: transparent;
		}

		td {
			display: grid;
			grid-template-columns: minmax(6.5rem, 38%) minmax(0, 1fr);
			align-items: baseline;
			gap: var(--space-3);
			padding: 0.3rem 0;
			border: 0;
			text-align: left;
			white-space: normal;
		}

		td.num {
			text-align: left;
		}

		td::before {
			content: attr(data-label);
			color: var(--text-muted);
			font-family: var(--font-mono);
			font-size: 0.6875rem;
			font-weight: 500;
			letter-spacing: 0.1em;
			text-transform: uppercase;
		}

		/* Values keep their own width: a state badge is not stretched across the card. */
		td > :global(*) {
			justify-self: start;
		}

		td[data-label='']::before {
			display: none;
		}

		td[data-label=''] {
			grid-template-columns: minmax(0, 1fr);
		}

		/* The first value is the card's title: the full width, clear of the actions button,
		   without its label (the hidden header row still names it for screen readers). */
		td:first-child {
			grid-template-columns: minmax(0, 1fr);
			padding: 0 2.75rem var(--space-2) 0;
		}

		td:first-child::before {
			display: none;
		}

		td:first-child .value {
			font-size: var(--text-md);
		}

		td.kind-code:first-child .value {
			font-size: 0.875rem;
		}

		td.kind-code .value {
			max-width: 100%;
		}

		td.wrap {
			min-width: 0;
		}

		td.actions {
			width: auto;
			padding: 0;
		}

		/* A menu sits in the card's corner; buttons go in a row at its foot. */
		td.actions.with-menu {
			position: absolute;
			top: var(--space-2);
			right: var(--space-2);
		}

		.row-actions {
			justify-content: flex-start;
			flex-wrap: wrap;
			margin-top: var(--space-2);
		}

		td.none {
			display: block;
		}
	}
</style>
