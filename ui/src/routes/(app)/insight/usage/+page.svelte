<script lang="ts">
	import BarChart from '#lib/components/BarChart.svelte';
	import Figures from '#lib/components/Figures.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportFilters from '#lib/components/ReportFilters.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import { formatCost, formatInteger } from '#lib/format.ts';
	import { formatValue, groupOptions, numberOf, type Row } from '#lib/report/format.ts';
	import { chartPoints, seriesRows } from '#lib/report/series.ts';
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
</script>

<div class="stack-lg">
	<PageHeader
		title="Usage and costs"
		description="What was called and what it cost, for the selected scope: the running periods, and a series over time."
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
					['none', 'provider', 'model', 'template', 'key_id', 'end_user', 'status', 'mode'],
					data.tagKeys
				)
			},
			...(data.tagKeys === null ? [{ name: 'group_tag', label: 'Tag key to split by' }] : []),
			{ name: 'model', label: 'Model' },
			{ name: 'template', label: 'Template' },
			{ name: 'end_user', label: 'End user' }
		]}
	/>

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
	.charts {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(18rem, 1fr));
		gap: var(--space-4);
	}
</style>
