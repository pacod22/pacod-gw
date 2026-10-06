<script lang="ts">
	import BarChart from '#lib/components/BarChart.svelte';
	import Figures from '#lib/components/Figures.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportFilters from '#lib/components/ReportFilters.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import ShareBars from '#lib/components/ShareBars.svelte';
	import TimeChart from '#lib/components/TimeChart.svelte';
	import { formatCost, formatInteger } from '#lib/format.ts';
	import {
		COMPLEXITIES,
		formatValue,
		groupLabel,
		groupOptions,
		numberOf,
		rowsOf,
		type Row
	} from '#lib/report/format.ts';
	import { chartPoints, seriesRows } from '#lib/report/series.ts';
	import {
		breakdownHref,
		bucketSearch,
		shareRows,
		stackSeries,
		type Measure
	} from '#lib/report/stack.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const bucket = $derived(data.usage.filters.bucket ?? 'hour');
	const grouped = $derived(String(data.usage.data?.group_by ?? 'none') !== 'none');
	const periods = $derived.by(() => {
		const all = (data.overview.data?.periods ?? {}) as Record<string, Row>;
		return ['hour', 'day', 'week', 'month']
			.filter((p) => all[p])
			.map((p) => ({ period: `This ${p}`, ...all[p] }));
	});
	const accuracy = $derived(data.accuracy.data);
	const select = (values: string[]) =>
		values.map((value) => ({ value, label: value.replaceAll('_', ' ') }));

	// The overview: groups stacked over time, by model unless the page is split otherwise.
	const MEASURES: { value: Measure; label: string }[] = [
		{ value: 'cost', label: 'Cost' },
		{ value: 'tokens', label: 'Tokens' },
		{ value: 'requests', label: 'Requests' }
	];
	const BUCKETS = [
		{ value: 'hour', label: 'Hour', range: 'the last 48 hours' },
		{ value: 'day', label: 'Day', range: 'the last 30 days' },
		{ value: 'week', label: 'Week', range: 'the last 12 weeks' },
		{ value: 'month', label: 'Month', range: 'the last 12 months' }
	];
	let measure = $state<Measure>('cost');
	const stackBy = $derived(groupLabel(String(data.stack.data?.group_by ?? 'model')));
	const stack = $derived(stackSeries(data.stack.data, measure));
	const measureLabel = $derived(MEASURES.find((m) => m.value === measure)?.label ?? '');
	const formatMeasure = (v: number) =>
		measure === 'cost'
			? formatCost(v, '')
			: `${formatInteger(v)} ${measure === 'tokens' ? 'tokens' : 'requests'}`;
	const formatAxis = (v: number) => (measure === 'cost' ? formatCost(v, '') : formatInteger(v));
</script>

<div class="stack-lg">
	<PageHeader
		title="Usage and costs"
		description="What was called and what it cost, for the selected scope: an overview by model, the running periods, and a series over time."
	/>
	<ReportFilters
		values={data.usage.filters}
		tagKeys={data.tagKeys}
		csv="/insight/usage/export{data.search}"
		controls={[
			{ name: 'from', label: 'From (UTC)', type: 'time' },
			{ name: 'to', label: 'To (UTC)', type: 'time' },
			{
				name: 'bucket',
				label: 'Per',
				type: 'select',
				options: select(['hour', 'day', 'week', 'month'])
			},
			{
				name: 'group_by',
				label: 'Split by',
				type: 'select',
				options: groupOptions(
					[
						'none',
						'provider',
						'model',
						'template',
						'key_id',
						'end_user',
						'status',
						'mode',
						'agent_id',
						'user_id',
						'task',
						'complexity'
					],
					data.tagKeys
				)
			},
			...(data.tagKeys === null ? [{ name: 'group_tag', label: 'Tag key to split by' }] : []),
			{ name: 'model', label: 'Model' },
			{ name: 'template', label: 'Template' },
			{ name: 'end_user', label: 'End user' },
			{ name: 'agent_id', label: 'Agent' },
			{ name: 'user_id', label: 'User' },
			{ name: 'task', label: 'Task' },
			{
				name: 'complexity',
				label: 'Complexity',
				type: 'select',
				empty: 'Any complexity',
				options: select(COMPLEXITIES)
			}
		]}
	/>

	<section class="stack">
		<h2>Overview</h2>
		<div class="card stack">
			<div class="chart-head">
				<h3>{measureLabel} per {bucket} by {stackBy}</h3>
				<div class="switches">
					<div class="segments" role="group" aria-label="Measure">
						{#each MEASURES as option (option.value)}
							<button
								type="button"
								aria-pressed={measure === option.value}
								onclick={() => (measure = option.value)}>{option.label}</button
							>
						{/each}
					</div>
					<nav class="segments" aria-label="Per">
						{#each BUCKETS as option (option.value)}
							<a
								href={bucketSearch(data.search, data.stack.data?.to, option.value)}
								aria-current={bucket === option.value ? 'page' : undefined}
								title="Per {option.value} over {option.range}">{option.label}</a
							>
						{/each}
					</nav>
				</div>
			</div>
			{#if data.stack.problem}
				<ProblemNotice problem={data.stack.problem} />
			{:else}
				<TimeChart
					stacked
					label="{measureLabel} per {bucket} by {stackBy}"
					labels={stack.labels}
					series={stack.series}
					format={formatMeasure}
					axisFormat={formatAxis}
				/>
			{/if}
		</div>
		<div class="charts">
			{#each [{ by: 'provider', report: data.byProvider }, { by: 'model', report: data.byModel }] as share (share.by)}
				<div class="card">
					{#if share.report.problem}
						<ProblemNotice problem={share.report.problem} />
					{:else}
						<ShareBars
							title="Cost by {share.by}"
							rows={shareRows(rowsOf(share.report.data, 'rows'))}
							more={breakdownHref(data.search, share.by)}
						/>
					{/if}
				</div>
			{/each}
		</div>
	</section>

	{#if data.overview.problem}
		<ProblemNotice problem={data.overview.problem} />
	{:else}
		<section class="stack">
			<h2>Running periods</h2>
			<ReportTable
				caption="Running periods"
				rows={periods}
				key="period"
				fields={[
					'period',
					'from',
					'requests',
					'errors',
					'input_tokens',
					'output_tokens',
					'cache_read_tokens',
					'reasoning_tokens',
					'cost'
				]}
				labels={{ from: 'Since (UTC)' }}
				empty="Nothing was recorded yet."
			/>
		</section>
	{/if}

	{#if data.usage.problem}
		<ProblemNotice problem={data.usage.problem} />
	{:else}
		<section class="stack">
			<h2>Over time</h2>
			<div class="charts">
				<div class="card">
					<BarChart
						title="Requests per {bucket}"
						points={chartPoints(data.usage.data, 'requests', bucket)}
						format={(v) => `${formatInteger(v)} requests`}
					/>
				</div>
				<div class="card">
					<BarChart
						title="Cost per {bucket}"
						points={chartPoints(data.usage.data, 'cost', bucket)}
						format={(v) => formatCost(v, '')}
					/>
				</div>
			</div>
			<ReportTable
				caption="Usage over time"
				rows={seriesRows(data.usage.data)}
				fields={[
					'bucket',
					// Without a split there is one series and no group to name.
					...(grouped ? ['group'] : []),
					'requests',
					'errors',
					'input_tokens',
					'output_tokens',
					'cost',
					'gateway_delay_p50_ms',
					'gateway_delay_p99_ms'
				]}
				empty="No calls in this range."
			/>
		</section>
	{/if}

	{#if accuracy}
		<section class="stack">
			<h2>How exact the figures are</h2>
			<Figures
				figures={[
					{ label: 'Calls', value: formatValue('requests', accuracy.requests) },
					{
						label: 'Counted by the gateway',
						value: formatValue('requests', accuracy.non_provider_requests),
						note: `Usage estimated or missing: ${formatValue('non_provider_share', numberOf(accuracy, 'non_provider_share'))} of the cost`
					},
					{
						label: 'Calls without a price',
						value: formatValue('requests', accuracy.price_missing_requests)
					},
					{ label: 'Total cost', value: formatValue('cost', accuracy.total_cost) }
				]}
			/>
		</section>
	{:else if data.accuracy.problem}
		<ProblemNotice problem={data.accuracy.problem} />
	{/if}
</div>

<style>
	.chart-head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-3);
	}

	.chart-head h3 {
		margin: 0;
		font-size: var(--text-sm);
		font-weight: 650;
	}

	.switches {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	/* Two segmented switches: the measure is chosen here, the bucket reloads the page. */
	.segments {
		display: flex;
		gap: 2px;
		padding: 2px;
		border-radius: var(--radius);
		background: var(--surface-2);
	}

	.segments button,
	.segments a {
		padding: 0.25rem 0.625rem;
		border: 0;
		border-radius: calc(var(--radius) - 2px);
		background: none;
		color: var(--text-muted);
		font: inherit;
		font-size: var(--text-xs);
		font-weight: 600;
		text-decoration: none;
		cursor: pointer;
	}

	.segments button:hover,
	.segments a:hover {
		color: var(--text);
	}

	.segments [aria-pressed='true'],
	.segments [aria-current='page'] {
		background: var(--accent);
		color: var(--on-accent);
	}

	.charts {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(18rem, 1fr));
		gap: var(--space-4);
	}
</style>
