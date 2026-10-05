<script lang="ts">
	import Figures from '#lib/components/Figures.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import RequestRows from '#lib/components/RequestRows.svelte';
	import { formatValue, rowsOf, type Row } from '#lib/report/format.ts';
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
	{#if data.report.problem}
		<ProblemNotice problem={data.report.problem} />
	{:else}
		<Figures
			figures={['requests', 'errors', 'input_tokens', 'output_tokens', 'cost'].map((key) => ({
				label: key.replaceAll('_', ' '),
				value: formatValue(key, totals[key])
			}))}
		/>
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
			<RequestRows caption="Most recent calls" rows={rowsOf(data.report.data, 'recent_requests')} />
		</section>
	{/if}
</div>
