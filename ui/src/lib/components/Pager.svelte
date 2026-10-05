<script lang="ts">
	import { page } from '$app/state';
	import { pageLink } from '#lib/nav.ts';

	/**
	 * Cursor paging: the API hands out the cursor of the next page only, so the way back is
	 * the browser's Back button or the first page.
	 */
	interface Props {
		/** The cursor of the page that is shown; `null` on the first page. */
		cursor: string | null;
		nextCursor: string | null;
		/** How many rows this page shows. */
		count: number;
	}

	let { cursor, nextCursor, count }: Props = $props();

	const link = (next: string | null) => pageLink(page.url, next);
</script>

{#if cursor || nextCursor}
	<nav class="pager" aria-label="Pages">
		<span class="muted small">{count} {count === 1 ? 'row' : 'rows'} on this page</span>
		<div class="row">
			{#if cursor}<a class="btn" href={link(null)}>First page</a>{/if}
			{#if nextCursor}
				<a class="btn" href={link(nextCursor)}>Next page</a>
			{:else}
				<span class="muted small">This is the last page.</span>
			{/if}
		</div>
	</nav>
{/if}

<style>
	.pager {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-3);
	}
</style>
