<script lang="ts">
	import type { Schemas } from '#lib/api/types.ts';
	import DataTable from '#lib/components/DataTable.svelte';
	import PriceRates from '#lib/components/PriceRates.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import type { Column } from '#lib/components/table.ts';
	import { PRICING_TABS } from '#lib/nav.ts';
	import { rateText } from '#lib/prices.ts';
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
		{
			key: 'rates',
			label: 'Rates / 1M',
			value: rateText,
			sortValue: (p) => p.input ?? null
		},
		{ key: 'version', label: 'Version', kind: 'number' },
		{ key: 'source', label: 'Source' },
		{ key: 'effective_from', label: 'In effect since (UTC)', kind: 'datetime' },
		{
			key: 'confirmed_at',
			label: 'Last checked at the source (UTC)',
			kind: 'datetime',
			empty: 'Not checked'
		}
	];
</script>

<div class="stack-lg">
	{#if data.principal.platform_admin}<Tabs label="Pricing" tabs={PRICING_TABS} />{/if}
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
		<DataTable
			caption="Prices in effect"
			{columns}
			rows={data.prices}
			rowKey={(p) => p.model_id}
			filter={data.prices.length > 8}
			{cell}
			custom={['rates']}
		/>
	{/if}
</div>

{#snippet cell(price: Price)}
	<PriceRates {price} currency={price.currency} />
{/snippet}
