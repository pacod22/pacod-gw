<script lang="ts">
	import Figures from '#lib/components/Figures.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import RequestRows from '#lib/components/RequestRows.svelte';
	import { scopeQuery } from '#lib/nav.ts';
	import { formatValue, rowsOf, type Row } from '#lib/report/format.ts';
	import type { PageProps } from './$types';

	let { data, params }: PageProps = $props();
	const totals = $derived((data.report.data?.totals ?? {}) as Row);
	const query = $derived(scopeQuery(data.scope));
</script>

<div class="stack-lg">
	<PageHeader
		title="Trace {params.trace_id}"
		description="The calls of one unit of work, in time order, and their totals."
		crumbs={[{ label: 'Requests', href: `/insight/requests${query}` }]}
	/>
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
		<RequestRows caption="Calls of the trace" rows={rowsOf(data.report.data, 'requests')} {query} />
	{/if}
</div>
