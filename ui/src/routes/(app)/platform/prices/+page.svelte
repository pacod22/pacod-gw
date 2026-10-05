<script lang="ts">
	import type { Schemas } from '#lib/api/types.ts';
	import DataTable from '#lib/components/DataTable.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import type { Column } from '#lib/components/table.ts';
	import { otherRates, rate } from '#lib/prices.ts';
	import type { PageProps } from './$types';

	type Price = Schemas['PriceView'];

	let { data }: PageProps = $props();

	const columns: Column<Price>[] = [
		{
			key: 'model',
			label: 'Model',
			kind: 'code',
			href: (p) => `/platform/models/${p.model_id}/prices`
		},
		{ key: 'currency', label: 'Currency' },
		{ key: 'input', label: 'Input / 1M', value: (p) => rate(p.input) },
		{ key: 'output', label: 'Output / 1M', value: (p) => rate(p.output) },
		{ key: 'cache_read', label: 'Cache read / 1M', value: (p) => rate(p.cache_read) },
		{
			key: 'cache_write_5m',
			label: 'Cache write 5 min / 1M',
			value: (p) => rate(p.cache_write_5m)
		},
		{ key: 'other', label: 'Other rates', value: otherRates },
		{ key: 'version', label: 'Version', kind: 'number' },
		{ key: 'source', label: 'Source' },
		{ key: 'effective_from', label: 'In effect since (UTC)', kind: 'datetime' }
	];
</script>

<div class="stack-lg">
	<PageHeader
		title="Prices"
		description="The price in effect for each model. Token rates are per one million tokens. Open a model to see its earlier versions or to set a new price."
	/>
	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if data.prices.length === 0}
		<div class="card muted">
			No model has a price yet. Open a model on the <a href="/platform/models">Models</a> page and set
			its price there.
		</div>
	{:else}
		<DataTable caption="Prices in effect" {columns} rows={data.prices} rowKey={(p) => p.model_id} />
	{/if}
</div>
