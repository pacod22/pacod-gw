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
		font-size: var(--text-xs);
		font-weight: 650;
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	.value {
		font-size: var(--text-xl);
		font-weight: 650;
	}

	.note {
		color: var(--text-muted);
		font-size: var(--text-sm);
	}
</style>
