<script lang="ts">
	/**
	 * The cost per group as shares of the whole: the name, a bar as long as the share, and
	 * the figures written out. The bar only helps the eye.
	 */
	import { reportingCurrency } from '#lib/currency.ts';
	import { formatCost } from '#lib/format.ts';
	import type { Share } from '#lib/report/stack.ts';

	interface Props {
		title: string;
		rows: Share[];
		/** Where the full list is. */
		more?: string;
	}

	let { title, rows, more }: Props = $props();
	const uid = $props.id();

	const PERCENT = new Intl.NumberFormat('en-GB', { style: 'percent', maximumFractionDigits: 1 });
</script>

<figure class="shares">
	<figcaption id="{uid}-title">{title}</figcaption>
	{#if rows.length === 0}
		<p class="muted small">No cost in this range.</p>
	{:else}
		<ul aria-labelledby="{uid}-title">
			{#each rows as row (row.name)}
				<li>
					<div class="figures">
						<span class="name">{row.name}</span>
						<span
							><strong>{formatCost(row.cost, reportingCurrency())}</strong>
							<span class="muted">{PERCENT.format(row.share)}</span></span
						>
					</div>
					<div class="track" aria-hidden="true">
						<div class="fill" style:width="{Math.max(row.share * 100, 0.5)}%"></div>
					</div>
				</li>
			{/each}
		</ul>
	{/if}
	{#if more}<a class="small more" href={more}>Full breakdown →</a>{/if}
</figure>

<style>
	.shares {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		min-width: 0;
	}

	figcaption {
		font-size: var(--text-sm);
		font-weight: 650;
	}

	ul {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		margin: 0;
		padding: 0;
		list-style: none;
	}

	li {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}

	.figures {
		display: flex;
		justify-content: space-between;
		gap: var(--space-3);
		font-size: var(--text-sm);
		font-variant-numeric: tabular-nums;
	}

	.name {
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: var(--text-xs);
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.track {
		height: 0.5rem;
		border-radius: 999px;
		background: var(--accent-soft);
	}

	.fill {
		height: 100%;
		border-radius: inherit;
		background: var(--accent);
	}

	.more {
		align-self: flex-start;
	}
</style>
