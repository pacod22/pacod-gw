<script lang="ts">
	import Figures from '#lib/components/Figures.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import RequestRows from '#lib/components/RequestRows.svelte';
	import { timeline } from '#lib/insight/timeline.ts';
	import { scopeQuery } from '#lib/nav.ts';
	import { formatValue, rowsOf, type Row } from '#lib/report/format.ts';
	import type { PageProps } from './$types';

	let { data, params }: PageProps = $props();
	const totals = $derived((data.report.data?.totals ?? {}) as Row);
	const query = $derived(scopeQuery(data.scope));
	const requests = $derived(rowsOf(data.report.data, 'requests'));
	const toolCalls = $derived(rowsOf(data.report.data, 'tool_calls'));
	const steps = $derived(timeline(requests, toolCalls));
</script>

<div class="stack-lg">
	<PageHeader
		title="Trace {params.trace_id}"
		description="The calls of one unit of work, in time order, and their totals: the totals are the model calls'; tool and agent calls use no tokens."
		crumbs={[{ label: 'Requests', href: `/insight/requests${query}` }]}
	>
		{#snippet actions()}
			<a
				class="btn btn-quiet"
				href="/insight/traces/{encodeURIComponent(params.trace_id)}/export{query}">Download CSV</a
			>
			<a
				class="btn btn-quiet"
				href="/insight/requests/jsonl?trace_id={encodeURIComponent(params.trace_id)}&audit=true"
				download
				data-sveltekit-reload>Export as JSON lines</a
			>
		{/snippet}
	</PageHeader>
	{#if data.report.problem}
		<ProblemNotice problem={data.report.problem} />
	{:else}
		<Figures
			figures={[
				'requests',
				'errors',
				'input_tokens',
				'output_tokens',
				'cost',
				'gateway_delay_ms'
			].map((key) => ({
				label: key === 'gateway_delay_ms' ? 'Gateway delay, summed' : key.replaceAll('_', ' '),
				value: formatValue(key, totals[key])
			}))}
		/>
		{#if toolCalls.length > 0}
			<ReportTable
				caption="Timeline: model, tool and agent calls"
				rows={steps}
				fields={['timestamp', 'kind', 'call', 'status', 'total_ms']}
				labels={{ kind: 'Kind', call: 'Call', total_ms: 'Took' }}
				href={(row) =>
					typeof row.request_id === 'string'
						? `/insight/requests/${encodeURIComponent(row.request_id)}${query}`
						: null}
				empty="No call."
			/>
		{/if}
		<RequestRows caption="Model calls of the trace" rows={requests} {query} />
	{/if}
</div>
