<script lang="ts">
	import ActivityCharts from '#lib/components/ActivityCharts.svelte';
	import Figures from '#lib/components/Figures.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportFilters from '#lib/components/ReportFilters.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import RequestRows from '#lib/components/RequestRows.svelte';
	import { headlineFigures, RANGES } from '#lib/report/analytics.ts';
	import { rowsOf, type Row } from '#lib/report/format.ts';
	import { scopeQuery } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, params }: PageProps = $props();
	const totals = $derived((data.report.data?.totals ?? {}) as Row);
</script>

<div class="stack-lg">
	<PageHeader
		title="End user {params.id}"
		crumbs={[{ label: 'End users', href: `/insight/end-users${scopeQuery(data.scope)}` }]}
	/>
	<ReportFilters
		values={data.filters}
		controls={[
			{
				name: 'range',
				label: 'Range',
				type: 'select',
				options: RANGES.map((r) => ({ value: r.id, label: r.label }))
			}
		]}
	/>
	{#if data.report.problem}
		<ProblemNotice problem={data.report.problem} />
	{:else}
		<Figures figures={headlineFigures(totals)} />
		{#if data.usage.problem}
			<ProblemNotice problem={data.usage.problem} />
		{:else}
			<section class="stack">
				<h2>Activity over time</h2>
				<ActivityCharts data={data.usage.data} />
			</section>
		{/if}
		<section class="stack">
			<h2>By model</h2>
			<ReportTable
				caption="By model"
				rows={rowsOf(data.report.data, 'by_model')}
				key="model"
				empty="No calls in this range."
			/>
		</section>
		<section class="stack">
			<h2>Most recent calls</h2>
			<RequestRows
				caption="Most recent calls"
				rows={rowsOf(data.report.data, 'recent_requests')}
				query={scopeQuery(data.scope)}
			/>
		</section>
	{/if}
</div>
