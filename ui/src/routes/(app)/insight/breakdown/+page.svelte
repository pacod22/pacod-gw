<script lang="ts">
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportFilters from '#lib/components/ReportFilters.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import { groupLabel, groupOptions, rowsOf } from '#lib/report/format.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();
	// What the report was grouped by, as it answered; the form's choice until then.
	const by = $derived(String(data.report.data?.by ?? data.report.filters.by ?? 'model'));
</script>

<div class="stack-lg">
	<PageHeader
		title="Breakdown"
		description="Totals per group, the most expensive first: by model, template, key, end user, provider, kind of error, agent, user, task or complexity, or by a tag such as the purpose of a call."
	/>
	<ReportFilters
		values={data.report.filters}
		tagKeys={data.tagKeys}
		csv="/insight/breakdown/export{data.search}"
		controls={[
			{ name: 'from', label: 'From (UTC)', type: 'time' },
			{ name: 'to', label: 'To (UTC)', type: 'time' },
			{
				name: 'by',
				label: 'By',
				type: 'select',
				options: groupOptions(
					[
						'model',
						'template',
						'key_id',
						'end_user',
						'provider',
						'error_type',
						'agent_id',
						'user_id',
						'task',
						'complexity',
						'routing_scheme',
						'routing_rule'
					],
					data.tagKeys
				)
			},
			...(data.tagKeys === null ? [{ name: 'group_tag', label: 'Tag key to group by' }] : [])
		]}
	/>
	{#if data.report.problem}
		<ProblemNotice problem={data.report.problem} />
	{:else}
		<ReportTable
			caption="Breakdown by {groupLabel(by)}"
			rows={rowsOf(data.report.data, 'rows')}
			key="group"
			labels={{ group: groupLabel(by) }}
			empty="No calls in this range."
		/>
	{/if}
</div>
