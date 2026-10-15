<script lang="ts">
	/**
	 * Measures over time, drawn with Chart.js: stacked columns of groups, or lines with
	 * columns among them. The legend below lists the series and hides one when it is pressed;
	 * the tooltip lists every series of a column.
	 * Colours are the theme's variables, resolved for the canvas and resolved again when the
	 * theme changes. The figures are also in the tables of the page.
	 */
	import {
		BarController,
		BarElement,
		CategoryScale,
		Chart,
		Filler,
		Legend,
		LinearScale,
		LineController,
		LineElement,
		PointElement,
		Tooltip
	} from 'chart.js';
	import type { ChartSeries } from '#lib/report/stack.ts';
	import { SvelteSet } from 'svelte/reactivity';
	import { axisColours, colourResolver, onThemeChange, withAlpha } from './chart-theme.ts';

	Chart.register(
		BarController,
		BarElement,
		LineController,
		LineElement,
		PointElement,
		CategoryScale,
		LinearScale,
		Filler,
		Tooltip,
		Legend
	);

	interface Props {
		/** Names the chart for a screen reader. */
		label: string;
		labels: string[];
		series: ChartSeries[];
		/** Series on top of each other: columns, or areas with `lines`. */
		stacked?: boolean;
		/** Stacked series as areas, not columns. */
		lines?: boolean;
		/** Shades the area under a line that is not stacked. */
		fill?: boolean;
		/** Writes a value in the tooltip: `12 requests`, `0.25`. */
		format: (value: number) => string;
		/** Writes a value on the axis; `format` when not given. */
		axisFormat?: (value: number) => string;
		/** The height of the plot in pixels. */
		height?: number;
		/** Data arriving every second: drawn without animation, which would never settle. */
		live?: boolean;
		/** What to say while there is nothing to draw. */
		emptyText?: string;
	}

	let {
		label,
		labels,
		series,
		stacked = false,
		lines = false,
		fill = true,
		format,
		axisFormat,
		height = 280,
		live = false,
		emptyText = 'Nothing to show for this range.'
	}: Props = $props();

	let canvas = $state<HTMLCanvasElement | null>(null);
	/** Counts theme changes, so the chart is drawn again with the new colours. */
	let theme = $state(0);
	// Plain handles: reading or writing them must not run the effect again.
	let chart: Chart | null = null;
	let chartCanvas: HTMLCanvasElement | null = null;
	let chartTheme = -1;

	const empty = $derived(
		labels.length === 0 || series.every((s) => s.values.every((v) => v === 0 || v === null))
	);

	/** The series hidden from the plot, by name; they stay in the legend. */
	const hidden = new SvelteSet<string>();
	const toggle = (name: string) => (hidden.has(name) ? hidden.delete(name) : hidden.add(name));

	function destroy() {
		chart?.destroy();
		chart = null;
		chartCanvas = null;
	}

	// Effects run in the browser only, so the chart is never built on the server.
	$effect(() => onThemeChange(() => theme++));

	$effect(() => {
		if (!canvas) {
			destroy();
			return;
		}
		const tooltipFormat = format;
		const tickFormat = axisFormat ?? format;
		const { colour, done } = colourResolver(canvas.parentElement ?? document.body);
		const datasets = series.map((s, index) => {
			const solid = colour(s.colour);
			const bar = (stacked && !lines) || s.bar === true;
			return {
				type: bar ? ('bar' as const) : ('line' as const),
				label: s.name,
				data: [...s.values],
				hidden: hidden.has(s.name),
				borderColor: solid,
				backgroundColor: bar ? solid : withAlpha(solid, stacked ? 0.35 : 0.13),
				borderWidth: bar ? 0 : stacked ? 1 : 2,
				pointRadius: 0,
				pointHoverRadius: 3,
				tension: 0.25,
				// A stacked area reaches down to the one below it.
				fill: bar ? false : stacked ? (index === 0 ? 'origin' : '-1') : fill,
				// Columns are drawn behind the lines.
				order: bar ? 1 : 0,
				...(stacked ? { stack: 'measure' } : {})
			};
		});
		const { muted, border } = axisColours(colour);
		done();
		const family = getComputedStyle(canvas).fontFamily;

		if (chart && (chartCanvas !== canvas || chartTheme !== theme)) destroy();

		if (!chart) {
			chartCanvas = canvas;
			chartTheme = theme;
			chart = new Chart(canvas, {
				type: 'bar',
				data: { labels: [...labels], datasets },
				options: {
					responsive: true,
					maintainAspectRatio: false,
					animation: live ? false : undefined,
					interaction: { mode: 'index', intersect: false },
					font: { family },
					color: muted,
					plugins: {
						// The legend is HTML below the plot: it wraps and scrolls instead of
						// taking the plot's height when there are many series.
						legend: { display: false },
						tooltip: {
							callbacks: { label: (c) => `${c.dataset.label}: ${tooltipFormat(Number(c.raw))}` }
						}
					},
					scales: {
						x: {
							stacked,
							grid: { display: false },
							border: { color: border },
							ticks: { autoSkip: true, maxRotation: 0, color: muted }
						},
						y: {
							stacked,
							beginAtZero: true,
							grid: { color: border },
							border: { display: false },
							ticks: { color: muted, callback: (v) => tickFormat(Number(v)) }
						}
					}
				}
			});
		} else {
			chart.data.labels = [...labels];
			chart.data.datasets = datasets;
			const tooltip = chart.options.plugins?.tooltip;
			if (tooltip?.callbacks)
				tooltip.callbacks.label = (c) => `${c.dataset.label}: ${tooltipFormat(Number(c.raw))}`;
			const y = chart.options.scales?.y;
			if (y?.ticks) y.ticks.callback = (v) => tickFormat(Number(v));
			chart.update(live ? 'none' : undefined);
		}
	});

	$effect(() => () => destroy());
</script>

{#if empty}
	<p class="muted small">{emptyText}</p>
{:else}
	<div class="chart">
		<div class="plot" role="img" aria-label={label} style:height="{height}px">
			<canvas bind:this={canvas}></canvas>
		</div>
		<ul class="legend" aria-label="Series of {label}">
			{#each series as s (s.name)}
				<li>
					<button
						type="button"
						aria-pressed={!hidden.has(s.name)}
						title={hidden.has(s.name) ? 'Show this series' : 'Hide this series'}
						onclick={() => toggle(s.name)}
					>
						<span
							class="swatch"
							class:bar={s.bar || (stacked && !lines)}
							style:--swatch="var({s.colour})"
						></span>
						{s.name}
					</button>
				</li>
			{/each}
		</ul>
	</div>
{/if}

<style>
	.chart {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		min-width: 0;
	}

	.legend {
		display: flex;
		flex-wrap: wrap;
		justify-content: center;
		gap: var(--space-1) var(--space-3);
		max-height: 4.5rem;
		margin: 0;
		padding: 0;
		overflow-y: auto;
		list-style: none;
	}

	.legend button {
		display: inline-flex;
		align-items: center;
		gap: var(--space-1);
		padding: 0;
		border: 0;
		background: none;
		color: var(--text);
		font: inherit;
		font-size: var(--text-xs);
		cursor: pointer;
	}

	.legend button[aria-pressed='false'] {
		color: var(--text-faint);
		text-decoration: line-through;
	}

	.swatch {
		width: 0.75rem;
		height: 0.1875rem;
		border-radius: 2px;
		background: var(--swatch);
	}

	.swatch.bar {
		height: 0.75rem;
	}

	[aria-pressed='false'] .swatch {
		opacity: 0.35;
	}

	.plot {
		position: relative;
		min-width: 0;
	}
</style>
