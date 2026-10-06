<script lang="ts">
	import ActivityCharts from '#lib/components/ActivityCharts.svelte';
	import Figures from '#lib/components/Figures.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportFilters from '#lib/components/ReportFilters.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import { formatInteger } from '#lib/format.ts';
	import { scopeQuery } from '#lib/nav.ts';
	import {
		choiceValues,
		headlineFigures,
		RANGES,
		totalsOf,
		withErrorRate
	} from '#lib/report/analytics.ts';
	import { COMPLEXITIES, rowsOf } from '#lib/report/format.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const TOP_END_USERS = 10;
	const endUsers = $derived(rowsOf(data.endUsers.data, 'rows'));
	const figures = $derived([
		...(data.endUsers.problem
			? []
			: [
					{
						label: 'End users',
						value: formatInteger(endUsers.length) + (endUsers.length === 1000 ? '+' : ''),
						note: 'Named in the calls'
					}
				]),
		...headlineFigures(totalsOf(data.usage.data), rowsOf(data.errors.data, 'by_type'))
	]);
	const options = (values: string[]) => values.map((value) => ({ value, label: value }));
	const brief = (items: { id: string; label: string }[]) =>
		items.map((item) => ({ value: item.id, label: item.label }));
	const tagValues = $derived(
		Object.fromEntries(
			Object.entries(data.choices.tags).map(([key, report]) => [key, choiceValues(report.data)])
		)
	);
	const endUserHref = (id: unknown) =>
		`/insight/end-users/${encodeURIComponent(String(id))}${scopeQuery(data.scope)}`;
</script>

<div class="stack-lg">
	<PageHeader
		title="Analytics"
		description="Who uses what: calls, tokens, cost and failures over time, per model and per end user, for the selected scope."
	/>
	<div class="stack">
		<ReportFilters
			values={data.filters}
			tagKeys={data.tagKeys ?? undefined}
			{tagValues}
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
					// The workspaces and their tags belong to the organisation chosen before.
					resets: ['workspace_id', 'tag.*']
				},
				{
					name: 'workspace_id',
					label: 'Workspace',
					type: 'select',
					empty: 'All workspaces',
					options: brief(data.workspaces),
					resets: ['tag.*'],
					...(data.scope.org ? {} : { disabled: 'Choose an organisation' })
				},
				{
					name: 'model',
					label: 'Model',
					type: 'select',
					empty: 'All models',
					options: options(choiceValues(data.choices.models.data))
				},
				{
					name: 'template',
					label: 'Template',
					type: 'select',
					empty: 'All templates',
					options: options(choiceValues(data.choices.templates.data))
				},
				{
					name: 'end_user',
					label: 'End user',
					type: 'select',
					empty: 'All end users',
					options: options(choiceValues(data.choices.endUsers.data, 'end_user'))
				},
				{
					name: 'agent_id',
					label: 'Agent',
					type: 'select',
					empty: 'All agents',
					options: options(choiceValues(data.choices.agents.data))
				},
				{
					name: 'task',
					label: 'Task',
					type: 'select',
					empty: 'All tasks',
					options: options(choiceValues(data.choices.tasks.data))
				},
				{
					name: 'complexity',
					label: 'Complexity',
					type: 'select',
					empty: 'Any complexity',
					options: options(COMPLEXITIES)
				}
			]}
		/>
		{#if !data.scope.ws}
			<p class="muted small">Tag filters are offered once a workspace is chosen.</p>
		{/if}
	</div>

	{#if data.usage.problem}
		<ProblemNotice problem={data.usage.problem} />
	{:else}
		<Figures {figures} />
		<ActivityCharts data={data.usage.data} cost />
	{/if}

	<section class="stack">
		<h2>By model</h2>
		{#if data.byModel.problem}
			<ProblemNotice problem={data.byModel.problem} />
		{:else}
			<ReportTable
				caption="By model"
				rows={withErrorRate(rowsOf(data.byModel.data, 'rows'))}
				key="group"
				fields={[
					'group',
					'requests',
					'input_tokens',
					'output_tokens',
					'cost',
					'errors',
					'error_rate'
				]}
				labels={{
					group: 'Model',
					requests: 'Calls',
					errors: 'Failed calls',
					error_rate: 'Failure rate'
				}}
				empty="No calls in this range."
			/>
		{/if}
	</section>

	<section class="stack">
		<div class="heading">
			<h2>Top end users</h2>
			<a class="small" href="/insight/end-users{scopeQuery(data.scope)}">All end users →</a>
		</div>
		{#if data.endUsers.problem}
			<ProblemNotice problem={data.endUsers.problem} />
		{:else}
			<ReportTable
				caption="Top end users, by cost"
				rows={endUsers.slice(0, TOP_END_USERS)}
				key="end_user"
				href={(row) => endUserHref(row.end_user)}
				fields={[
					'end_user',
					'requests',
					'errors',
					'input_tokens',
					'output_tokens',
					'cost',
					'last_request_at'
				]}
				labels={{ requests: 'Calls', errors: 'Failed calls' }}
				empty="No call in this range named an end user."
			/>
		{/if}
	</section>
</div>

<style>
	.heading {
		display: flex;
		align-items: baseline;
		justify-content: space-between;
		gap: var(--space-3);
	}
</style>
