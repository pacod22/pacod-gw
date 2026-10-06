<script lang="ts">
	/** Headline figures: a label, the number, and what it refers to. */
	interface Figure {
		label: string;
		value: string;
		note?: string;
	}

	let { figures }: { figures: Figure[] } = $props();
</script>

<dl class="figures">
	{#each figures as figure (figure.label)}
		<div class="card figure">
			<dt>{figure.label}</dt>
			<dd class="value">{figure.value || '–'}</dd>
			{#if figure.note}<dd class="note">{figure.note}</dd>{/if}
		</div>
	{/each}
</dl>

<style>
	.figures {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(11rem, 1fr));
		gap: var(--space-3);
	}

	.figure {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}

	dt {
		color: var(--text-muted);
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 500;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}

	.value {
		font-family: var(--font-display);
		font-size: 1.5rem;
		font-weight: 700;
		letter-spacing: -0.02em;
		font-variant-numeric: tabular-nums;
	}

	.note {
		color: var(--text-muted);
		font-size: var(--text-sm);
	}
</style>
