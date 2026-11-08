<script lang="ts">
	import PageHeader from '#lib/components/PageHeader.svelte';
	import Pager from '#lib/components/Pager.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportFilters from '#lib/components/ReportFilters.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import { scopeQuery } from '#lib/nav.ts';
	import { rowsOf } from '#lib/report/format.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const rows = $derived(rowsOf(data.report.data, 'tool_calls'));
	const next = $derived(
		typeof data.report.data?.next_cursor === 'string' ? data.report.data.next_cursor : null
	);
	const query = $derived(scopeQuery(data.scope));
	const choice = (value: string) => ({ value, label: value || 'Any' });
</script>

<div class="stack-lg">
	<PageHeader
		title="Tool calls"
		description="Calls of tools (/mcp) and agents (/a2a) through the gateway, newest first: who called what, what the gateway decided and how it went. Arguments are not kept; calls with the same arguments share their hash."
	/>
	<ReportFilters
		values={data.report.filters}
		controls={[
			{ name: 'from', label: 'From (UTC)', type: 'time' },
			{ name: 'to', label: 'To (UTC)', type: 'time' },
			{ name: 'agent_id', label: 'Agent' },
			{ name: 'target', label: 'Server or agent' },
			{ name: 'tool', label: 'Tool' },
			{
				name: 'verdict',
				label: 'Verdict',
				type: 'select',
				options: ['', 'allowed', 'denied', 'limited', 'invalid'].map(choice)
			},
			{
				name: 'status',
				label: 'Status',
				type: 'select',
				options: ['', 'ok', 'error', 'timeout', 'cancelled', 'rejected'].map(choice)
			},
			{ name: 'trace_id', label: 'Trace id' },
			{ name: 'end_user', label: 'End user' }
		]}
	/>
	{#if data.report.problem}
		<ProblemNotice problem={data.report.problem} />
	{:else}
		<ReportTable
			caption="Tool calls"
			{rows}
			key="id"
			fields={[
				'timestamp',
				'agent_id',
				'protocol',
				'target',
				'exposed_name',
				'operation',
				'verdict',
				'status',
				'result_error',
				'total_ms',
				'response_bytes',
				'trace_id'
			]}
			labels={{
				agent_id: 'Agent',
				target: 'Server or agent',
				exposed_name: 'Tool',
				operation: 'Operation',
				result_error: 'Result is an error',
				total_ms: 'Took',
				response_bytes: 'Answer bytes'
			}}
			href={(row) =>
				typeof row.trace_id === 'string' && row.trace_id
					? `/insight/traces/${encodeURIComponent(row.trace_id)}${query}`
					: null}
			empty="No tool or agent was called through the gateway in this range."
		/>
		<Pager cursor={data.cursor} nextCursor={next} count={rows.length} />
	{/if}
</div>
