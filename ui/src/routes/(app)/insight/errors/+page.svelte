<script lang="ts">
	import BarChart from '#lib/components/BarChart.svelte';
	import Figures from '#lib/components/Figures.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportFilters from '#lib/components/ReportFilters.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import RequestRows from '#lib/components/RequestRows.svelte';
	import { formatInteger } from '#lib/format.ts';
	import { scopeQuery } from '#lib/nav.ts';
	import { bucketLabel, formatValue, numberOf, rowsOf, type Row } from '#lib/report/format.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();
	const report = $derived(data.report.data);
	const totals = $derived((report?.totals ?? {}) as Row);
	const bucket = $derived(String(report?.bucket ?? 'hour'));
</script>

<div class="stack-lg">
	<PageHeader
		title="Errors"
		description="Calls that did not end well: how many, of which kind, on which provider and model, and the most recent ones."
	/>
	<ReportFilters
		values={data.report.filters}
		csv="/insight/errors/export{data.search}"
		controls={[
			{ name: 'from', label: 'From (UTC)', type: 'time' },
			{ name: 'to', label: 'To (UTC)', type: 'time' },
			{ name: 'provider', label: 'Provider' },
			{ name: 'model', label: 'Model' }
		]}
	/>
	{#if data.report.problem}
		<ProblemNotice problem={data.report.problem} />
	{:else if report}
		<Figures
			figures={[
				{ label: 'Calls', value: formatValue('requests', totals.requests) },
				{ label: 'Errors', value: formatValue('errors', totals.errors) },
				{ label: 'Error rate', value: formatValue('error_rate', totals.error_rate) }
			]}
		/>
		<div class="card">
			<BarChart
				title="Errors per {bucket}"
				points={rowsOf(report, 'over_time').map((row) => ({
					label: bucketLabel(row.bucket, bucket),
					value: numberOf(row, 'errors')
				}))}
				format={(v) => `${formatInteger(v)} errors`}
			/>
		</div>
		<section class="stack">
			<h2>By kind</h2>
			<ReportTable
				caption="Errors by kind"
				rows={rowsOf(report, 'by_type')}
				key="error_type"
				empty="No errors in this range."
			/>
		</section>
		<section class="stack">
			<h2>By provider and model</h2>
			<ReportTable
				caption="Errors by provider and model"
				rows={rowsOf(report, 'by_provider_model')}
				empty="No calls in this range."
			/>
		</section>
		<section class="stack">
			<h2>Most recent failed calls</h2>
			<RequestRows
				caption="Most recent failed calls"
				rows={rowsOf(report, 'recent_requests')}
				empty="No failed calls in this range."
				query={scopeQuery(data.scope)}
			/>
		</section>
	{/if}
</div>
