<script lang="ts">
	import { rateList } from '../prices';

	/** The rates of one price as compact label–value pairs, per one million tokens. */
	interface Props {
		price: Parameters<typeof rateList>[0];
		currency?: string;
	}

	let { price, currency }: Props = $props();
	const rates = $derived(rateList(price));
</script>

{#if rates.length === 0}
	<span class="muted">Free</span>
{:else}
	<dl class="rates">
		{#each rates as { label, value }, i (i)}
			<div>
				<dt>{label}</dt>
				<dd>
					{value}{#if currency && i === 0}<span class="currency">{currency}</span>{/if}
				</dd>
			</div>
		{/each}
	</dl>
{/if}

<style>
	.rates {
		display: flex;
		flex-wrap: wrap;
		gap: 0.125rem 1rem;
		margin: 0;
		min-width: 14rem;
		white-space: normal;
	}

	.rates div {
		display: flex;
		align-items: baseline;
		gap: 0.375rem;
	}

	/* In the card layout of narrow screens the value column is narrower than that. */
	@media (max-width: 48rem) {
		.rates {
			min-width: 0;
		}
	}

	dt {
		color: var(--text-muted);
		font-size: var(--text-xs);
	}

	dd {
		margin: 0;
		font-variant-numeric: tabular-nums;
		white-space: nowrap;
	}

	.currency {
		margin-left: 0.25em;
		color: var(--text-muted);
		font-size: var(--text-xs);
	}
</style>
