<script lang="ts">
	/** Headline figures that lead to the page behind them: a label, the figure and a note. */
	interface Tile {
		label: string;
		value: string;
		note?: string;
		href: string;
	}

	let { tiles }: { tiles: Tile[] } = $props();
</script>

<ul class="tiles">
	{#each tiles as tile (tile.label)}
		<li>
			<a class="card tile" href={tile.href}>
				<span class="label">{tile.label}</span>
				<span class="value">{tile.value}</span>
				{#if tile.note}<span class="muted small">{tile.note}</span>{/if}
			</a>
		</li>
	{/each}
</ul>

<style>
	.tiles {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(10rem, 1fr));
		gap: var(--space-3);
		padding: 0;
		list-style: none;
	}

	.tile {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		height: 100%;
		color: inherit;
		text-decoration: none;
		transition: border-color 0.15s;
	}

	.tile:hover,
	.tile:focus-visible {
		border-color: var(--accent);
	}

	.label {
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
</style>
