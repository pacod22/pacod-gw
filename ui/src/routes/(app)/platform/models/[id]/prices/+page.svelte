<script lang="ts">
	import type { Schemas } from '#lib/api/types.ts';
	import ActionDialog from '#lib/components/ActionDialog.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import type { Column } from '#lib/components/table.ts';
	import { otherRates, rate } from '#lib/prices.ts';
	import type { PageProps } from './$types';

	type Price = Schemas['PriceView'];

	let { data, form }: PageProps = $props();
	let adding = $state(false);

	const current = $derived(data.prices.find((p) => p.in_effect));

	const columns: Column<Price>[] = [
		{ key: 'version', label: 'Version', kind: 'number' },
		{ key: 'in_effect', label: 'In effect', value: (p) => (p.in_effect ? 'Yes' : '') },
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'source', label: 'Source' },
		{ key: 'currency', label: 'Currency' },
		{ key: 'input', label: 'Input / 1M', value: (p) => rate(p.input) },
		{ key: 'output', label: 'Output / 1M', value: (p) => rate(p.output) },
		{ key: 'cache_read', label: 'Cache read / 1M', value: (p) => rate(p.cache_read) },
		{
			key: 'cache_write_5m',
			label: 'Cache write 5 min / 1M',
			value: (p) => rate(p.cache_write_5m)
		},
		{ key: 'cache_write_1h', label: 'Cache write 1 h / 1M', value: (p) => rate(p.cache_write_1h) },
		{ key: 'other', label: 'Other rates', value: otherRates },
		{ key: 'effective_from', label: 'Effective from (UTC)', kind: 'datetime' },
		{ key: 'evidence', label: 'Evidence' }
	];
</script>

<div class="stack-lg">
	<PageHeader
		title="Prices of {data.model?.name ?? 'model'}"
		description="Every version of the model's price, newest first. A new price is a new version: calls keep the price that was in effect when they were made."
		crumbs={[{ label: 'Models', href: '/platform/models' }]}
	>
		{#snippet actions()}
			{#if data.fields}
				<button type="button" class="btn btn-primary" onclick={() => (adding = true)}>
					Set a new price
				</button>
			{/if}
		{/snippet}
	</PageHeader>

	<OutcomeNotice outcome={form} />

	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if data.prices.length === 0}
		<div class="card stack muted">
			<p>
				{#if data.model?.free}
					This model is marked as free, so it can be active without a price.
				{:else}
					This model has no price yet. A draft cannot be activated without one: set a price, or mark
					the model as free.
				{/if}
			</p>
			{#if data.fields}
				<div>
					<button type="button" class="btn btn-primary" onclick={() => (adding = true)}>
						Set a new price
					</button>
				</div>
			{/if}
		</div>
	{:else}
		{#if !current}
			<p class="muted">No version is in effect at the moment.</p>
		{/if}
		<DataTable
			caption="Price versions"
			{columns}
			rows={data.prices}
			rowKey={(p) => p.version}
			{cell}
			custom={['evidence']}
		/>
	{/if}
</div>

{#snippet cell(price: Price)}
	{#if price.evidence_url}
		<a href={price.evidence_url} rel="noreferrer noopener" target="_blank">Source page</a>
	{:else if price.evidence_text}
		<span title={price.evidence_text}
			>{price.evidence_text.slice(0, 40)}{price.evidence_text.length > 40 ? '…' : ''}</span
		>
	{:else}
		<span class="muted">–</span>
	{/if}
{/snippet}

{#if data.fields}
	<ActionDialog
		bind:open={adding}
		title="Set a new price"
		action="?/price"
		submit="Save price"
		outcome={form}
		fields={data.fields}
		object={current}
		mode="create"
	>
		<p class="muted small">
			Token rates are per one million tokens. Leave a rate empty when the provider does not bill it.
			The form starts from the price in effect.
		</p>
	</ActionDialog>
{/if}
