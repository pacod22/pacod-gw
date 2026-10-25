<script lang="ts">
	import Figures from '#lib/components/Figures.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportFilters from '#lib/components/ReportFilters.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import TimeChart from '#lib/components/TimeChart.svelte';
	import { hasPlatformRole } from '#lib/access.ts';
	import { reportingCurrency } from '#lib/currency.ts';
	import { formatCost } from '#lib/format.ts';
	import { choiceValues, formatCompact, RANGES } from '#lib/report/analytics.ts';
	import { CACHE_RESULTS, cacheFigures, SAVED, withHitRate } from '#lib/report/cache.ts';
	import { rowsOf } from '#lib/report/format.ts';
	import { measureSeries } from '#lib/report/stack.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const report = $derived(data.cache.data);
	const totals = $derived((report?.totals ?? {}) as Record<string, unknown>);
	const figures = $derived(cacheFigures(totals, reportingCurrency()));
	const results = $derived(measureSeries(report, CACHE_RESULTS, 'series'));
	const saved = $derived(measureSeries(report, SAVED, 'series'));
	const workspaceName = $derived(
		new Map([...data.workspaces, ...data.orgs].map((item) => [item.id, item.label]))
	);
	const topWorkspaces = $derived(
		withHitRate(rowsOf(report, 'top_workspaces')).map((row) => ({
			...row,
			workspace: workspaceName.get(String(row.workspace_id)) ?? row.workspace_id,
			organisation: workspaceName.get(String(row.organisation_id)) ?? row.organisation_id
		}))
	);
	const options = (values: string[]) => values.map((value) => ({ value, label: value }));
	const brief = (items: { id: string; label: string }[]) =>
		items.map((item) => ({ value: item.id, label: item.label }));
	const FIELDS = ['hits', 'lookups', 'hit_rate', 'saved_cost'];
	const LABELS = {
		hits: 'Answered from the cache',
		lookups: 'Looked up',
		hit_rate: 'Hit rate',
		saved_cost: 'Saved'
	};
</script>

<div class="stack-lg">
	<PageHeader
		title="Response cache"
		description="What the response cache did with the calls of the selected scope: the share it answered from kept answers, and what that saved."
	/>
	<ReportFilters
		values={data.filters}
		controls={[
			{
				name: 'range',
				label: 'Range',
				type: 'select',
				options: RANGES.map((r) => ({ value: r.id, label: r.label }))
			},
			{
				name: 'organisation_id',
				label: 'Organisation',
				type: 'select',
				empty: 'All organisations',
				options: brief(data.orgs),
				resets: ['workspace_id']
			},
			{
				name: 'workspace_id',
				label: 'Workspace',
				type: 'select',
				empty: 'All workspaces',
				options: brief(data.workspaces),
				...(data.scope.org ? {} : { disabled: 'Choose an organisation' })
			},
			{
				name: 'model',
				label: 'Model',
				type: 'select',
				empty: 'All models',
				options: options(choiceValues(data.models.data))
			}
		]}
	/>

	{#if data.cache.problem}
		<ProblemNotice problem={data.cache.problem} />
	{:else}
		<Figures {figures} />
		<p class="muted small">
			Only the calls of workspaces with a response cache count. A call is looked up when it may be
			answered from the cache: by default, those at temperature 0.
			{#if hasPlatformRole(data.principal)}
				Where answers are kept, and clearing them, is under
				<a href="/platform/response-cache">Platform › Response cache</a>.
			{:else}
				Clearing the cache is on the Plugins page of the workspace or organisation.
			{/if}
		</p>
		<div class="charts">
			<figure class="card chart">
				<figcaption>What the cache did</figcaption>
				<TimeChart
					label="What the cache did"
					labels={results.labels}
					series={results.series}
					stacked
					format={formatCompact}
					height={220}
					emptyText="No call was looked up in this range."
				/>
			</figure>
			<figure class="card chart">
				<figcaption>Saved over time</figcaption>
				<TimeChart
					label="Saved over time"
					labels={saved.labels}
					series={saved.series}
					fill
					format={(v) => formatCost(v, reportingCurrency())}
					height={220}
					emptyText="Nothing was answered from the cache in this range."
				/>
			</figure>
		</div>

		<section class="stack">
			<h2>By model</h2>
			<ReportTable
				caption="By model, most hits first"
				rows={withHitRate(rowsOf(report, 'top_models'))}
				key="model"
				fields={['provider', 'model', ...FIELDS]}
				labels={LABELS}
				empty="No call was looked up in this range."
			/>
		</section>

		{#if !data.scope.ws}
			<section class="stack">
				<h2>By workspace</h2>
				<ReportTable
					caption="By workspace, most hits first"
					rows={topWorkspaces}
					key="workspace_id"
					fields={['organisation', 'workspace', ...FIELDS]}
					labels={LABELS}
					empty="No call was looked up in this range."
				/>
			</section>
		{/if}
	{/if}
</div>

<style>
	.charts {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(20rem, 1fr));
		gap: var(--space-4);
	}
	.chart {
		margin: 0;
	}
	figcaption {
		font-weight: 600;
		margin-bottom: var(--space-2);
	}
</style>
