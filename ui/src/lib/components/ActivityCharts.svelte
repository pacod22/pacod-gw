<script lang="ts">
	/**
	 * A usage report over time as PACOD's analytics shows it: calls with the failed ones among
	 * them, tokens in and out, and optionally the cost, side by side.
	 */
	import { reportingCurrency } from '#lib/currency.ts';
	import { formatCost } from '#lib/format.ts';
	import { CALLS_AND_FAILURES, COST, formatCompact, TOKENS } from '#lib/report/analytics.ts';
	import { measureSeries, type MeasureLine } from '#lib/report/stack.ts';
	import TimeChart from './TimeChart.svelte';

	interface Props {
		/** A usage report without a split. */
		data: unknown;
		/** Adds the cost over time. */
		cost?: boolean;
	}

	let { data, cost = false }: Props = $props();

	const charts = $derived(
		[
			{ title: 'Calls and failed calls', lines: CALLS_AND_FAILURES, format: formatCompact },
			{ title: 'Tokens over time', lines: TOKENS, format: formatCompact },
			...(cost
				? [
						{
							title: 'Cost over time',
							lines: COST,
							format: (v: number) => formatCost(v, reportingCurrency())
						}
					]
				: [])
		].map((chart: { title: string; lines: MeasureLine[]; format: (v: number) => string }) => ({
			...chart,
			...measureSeries(data, chart.lines)
		}))
	);
</script>

<div class="charts">
	{#each charts as chart (chart.title)}
		<figure class="card chart">
			<figcaption>{chart.title}</figcaption>
			<TimeChart
				label={chart.title}
				labels={chart.labels}
				series={chart.series}
				format={chart.format}
				height={220}
			/>
		</figure>
	{/each}
</div>

<style>
	.charts {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(20rem, 1fr));
		gap: var(--space-4);
	}

	.chart {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		min-width: 0;
		margin: 0;
	}

	figcaption {
		font-size: var(--text-sm);
		font-weight: 650;
	}
</style>
