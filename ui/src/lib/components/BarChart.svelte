<script lang="ts">
	/**
	 * One measure over time as columns: a single series, so the title names it and there is
	 * no legend. Hovering or focusing a column reads its value out; the figures are also in
	 * the table the page shows below the chart. Targets are dashed lines across it; one far
	 * above every column is named below the chart instead, so the columns keep their scale.
	 */
	interface Point {
		label: string;
		value: number;
	}

	interface Props {
		title: string;
		points: Point[];
		/** Writes a value: `12 requests`, `0.25 ms`. */
		format: (value: number) => string;
		targets?: Point[];
	}

	let { title, points, format, targets = [] }: Props = $props();
	const uid = $props.id();

	/** A target this far above the highest column is named, not drawn. */
	const DRAWN_UP_TO = 4;
	const highest = $derived(Math.max(0, ...points.map((p) => p.value)));
	const drawn = $derived(targets.filter((t) => t.value > 0 && t.value <= highest * DRAWN_UP_TO));
	const above = $derived(targets.filter((t) => t.value > 0 && !drawn.includes(t)));
	const top = $derived(Math.max(highest, ...drawn.map((t) => t.value)));
	let active = $state<number | null>(null);
	const shown = $derived(active === null ? null : (points[active] ?? null));
</script>

<figure class="chart">
	<figcaption id="{uid}-title">{title}</figcaption>
	{#if points.length === 0 || top === 0}
		<p class="muted small">Nothing to show for this range.</p>
	{:else}
		<p class="readout" aria-hidden="true">
			{#if shown}
				<strong>{format(shown.value)}</strong> <span>{shown.label}</span>
			{:else}
				<strong>{format(top)}</strong> <span>at the highest point</span>
			{/if}
		</p>
		<div class="plot" role="group" aria-labelledby="{uid}-title">
			{#each drawn as target, index (target.label)}
				<!-- Labels alternate sides, so that two targets close together stay readable. -->
				<span
					class="target"
					class:left={index % 2 === 1}
					style:bottom="{(target.value / top) * 100}%"
					aria-hidden="true"
				>
					<span>{target.label} {format(target.value)}</span>
				</span>
			{/each}
			{#each points as point, index (point.label)}
				<button
					type="button"
					class="slot"
					class:active={active === index}
					aria-label="{point.label}: {format(point.value)}"
					onpointerenter={() => (active = index)}
					onpointerleave={() => (active = null)}
					onfocus={() => (active = index)}
					onblur={() => (active = null)}
				>
					<span class="bar" style:height="{(point.value / top) * 100}%"></span>
				</button>
			{/each}
		</div>
		<div class="axis">
			<span>{points[0].label}</span>
			{#if points.length > 1}<span>{points[points.length - 1].label}</span>{/if}
		</div>
	{/if}
	{#if targets.length > 0}
		<p class="muted small">
			{targets.map((t) => `${t.label} ${format(t.value)}`).join(' · ')}{above.length > 0 &&
			points.length > 0
				? ` – ${above.length === targets.length ? 'all' : above.map((t) => t.label).join(', ')} far above every column`
				: ''}
		</p>
	{/if}
</figure>

<style>
	/* One series: the brand colour, which is stepped for each mode. */
	.chart {
		--series: var(--accent);
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		min-width: 0;
	}

	figcaption {
		font-size: var(--text-sm);
		font-weight: 650;
	}

	.readout {
		display: flex;
		align-items: baseline;
		gap: var(--space-2);
		color: var(--text-muted);
		font-size: var(--text-sm);
	}

	.readout strong {
		color: var(--text);
		font-size: var(--text-lg);
	}

	.plot {
		position: relative;
		display: flex;
		align-items: stretch;
		gap: 2px;
		height: 8rem;
		border-bottom: 1px solid var(--border);
		background: linear-gradient(var(--border), var(--border)) 0 0 / 100% 1px no-repeat;
	}

	/* The slot is the hit target; the bar inside it stays thin. */
	.slot {
		display: flex;
		flex: 1 1 0;
		align-items: flex-end;
		justify-content: center;
		min-width: 3px;
		padding: 0;
		border: 0;
		background: none;
		cursor: default;
	}

	.bar {
		width: 100%;
		max-width: 24px;
		min-height: 1px;
		border-radius: 4px 4px 0 0;
		background: var(--series);
	}

	.slot.active .bar,
	.slot:focus-visible .bar {
		outline: 2px solid var(--text);
		outline-offset: 1px;
	}

	.slot:focus-visible {
		outline: none;
	}

	.target {
		position: absolute;
		right: 0;
		left: 0;
		border-top: 1px dashed var(--text-muted);
		pointer-events: none;
	}

	.target.left span {
		right: auto;
		left: 0;
	}

	.target span {
		position: absolute;
		right: 0;
		bottom: 2px;
		padding: 0 var(--space-1);
		border-radius: 4px;
		background: var(--surface);
		color: var(--text-muted);
		font-size: var(--text-xs);
	}

	.axis {
		display: flex;
		justify-content: space-between;
		color: var(--text-muted);
		font-size: var(--text-xs);
	}
</style>
