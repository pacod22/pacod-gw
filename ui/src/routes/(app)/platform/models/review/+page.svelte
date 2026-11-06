<script lang="ts">
	import { enhance } from '$app/forms';
	import DataTable from '#lib/components/DataTable.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import PriceRates from '#lib/components/PriceRates.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import type { Column } from '#lib/components/table.ts';
	import { changeText } from '#lib/discovery.ts';
	import { MODEL_TABS } from '#lib/nav.ts';
	import {
		STEP_LABELS,
		STEPS,
		byProvider,
		reviewItems,
		stepCounts,
		trackedProviders,
		type ReviewItem
	} from '#lib/review.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	type Price = (typeof data.prices)[number];
	type Item = ReviewItem<Price>;

	const items: Item[] = $derived(
		reviewItems(data.proposals, data.prices, data.models, trackedProviders(data.job))
	);
	const groups = $derived(byProvider(items));
	const counts = $derived(stepCounts(items));

	/** What the row waits for, in a few words. */
	function next(item: Item): string {
		switch (item.step) {
			case 'new':
				return 'Add it as a draft, or dismiss it.';
			case 'needs_price':
				if (data.job === null) return 'Give it a price, or mark it free.';
				if (!data.job.enabled)
					return 'Set it by hand, or mark it free: the price tracker is switched off.';
				return item.tracked
					? 'The price tracker looks for one; you can also set it by hand.'
					: 'Set it by hand: the price tracker reads no page of this provider.';
			case 'price_proposed':
				return 'Approve the price, then activate the model.';
			case 'ready':
				return 'Check its capabilities, then activate it.';
			case 'price_change':
				return 'Approve or reject the new price.';
			case 'update':
				return 'Apply what the provider reports, or dismiss it to keep the model as it is.';
			case 'gone':
				return 'Deprecate or retire the model, or dismiss the flag if the provider still serves it.';
		}
	}

	const columns: Column<Item>[] = [
		{
			key: 'step',
			label: 'Step',
			value: (i) => STEP_LABELS[i.step],
			sortValue: (i) => STEPS.indexOf(i.step)
		},
		{
			key: 'name',
			label: 'Model',
			kind: 'code',
			href: (i) => (i.model_id ? `/platform/models/${i.model_id}` : null)
		},
		{ key: 'detail', label: 'Next', wrap: true, sortable: false },
		{ key: 'since', label: 'Waiting since (UTC)', kind: 'datetime', empty: '–' }
	];
</script>

<div class="stack-lg">
	<PageHeader
		title="Review queue"
		description="Everything about the repository that waits for you, in the order a new model goes through it: found by discovery, added as a draft, priced, activated. Price changes and models the providers no longer list wait here too."
	/>
	<Tabs label="Models" tabs={MODEL_TABS} />

	<OutcomeNotice outcome={form} always={['approve', 'reject', 'dismiss', 'apply']} />
	{#if data.problem}<ProblemNotice problem={data.problem} />{/if}

	<section class="card" aria-label="What waits">
		<dl class="facts">
			{#each STEPS as step (step)}
				<dt>{STEP_LABELS[step]}</dt>
				<dd>{counts[step]}</dd>
			{/each}
			<dt>Price tracker</dt>
			<dd>
				{#if data.job === null}
					Not known: the price job could not be read.
				{:else if !data.job.enabled}
					Switched off: drafts are priced by hand.
				{:else}
					Switched on. It prices a draft accepted from discovery at once, where it has a source for
					the provider.
				{/if}
				<a href="/platform/price-tracker">Price tracker</a>
			</dd>
		</dl>
	</section>

	{#if items.length === 0}
		<div class="card muted">Nothing waits: every model is decided, priced and in its state.</div>
	{:else}
		{#each groups as group (group.provider_id)}
			<section class="provider stack" aria-labelledby="group-{group.provider_id}">
				<h2 id="group-{group.provider_id}">
					{#if group.provider_id}
						<a href="/platform/providers/{group.provider_id}">{group.provider}</a>
					{:else}
						Unknown provider
					{/if}
					<span class="count">{group.items.length}</span>
				</h2>
				<DataTable
					caption="What waits at {group.provider || 'an unknown provider'}"
					rows={group.items}
					rowKey={(i) => i.key}
					{columns}
					{cell}
					custom={['detail']}
					{actions}
				/>
			</section>
		{/each}
	{/if}
</div>

{#snippet cell(item: Item)}
	{#if item.price}
		<span class="stacked">
			<PriceRates price={item.price} currency={item.price.currency} />
			<span class="muted small">{next(item)}</span>
		</span>
	{:else if item.changes.length > 0}
		<span class="stacked">
			{#each item.changes as change (change.field)}<span>{changeText(change)}</span>{/each}
			<span class="muted small">{next(item)}</span>
		</span>
	{:else}
		{next(item)}
	{/if}
{/snippet}

{#snippet actions(item: Item)}
	{#if item.step === 'new'}
		<a
			class="btn btn-quiet"
			href="/platform/models/discovery?q={encodeURIComponent(item.upstream_id)}"
			>Add as draft…<span class="visually-hidden"> {item.name}</span></a
		>
	{:else if item.step === 'needs_price' && item.model_id}
		<a class="btn btn-quiet" href="/platform/models/{item.model_id}/prices"
			>Set a price<span class="visually-hidden"> for {item.name}</span></a
		>
	{:else if item.step === 'ready' && item.model_id}
		<a class="btn btn-quiet" href="/platform/models/{item.model_id}"
			>Activate…<span class="visually-hidden"> {item.name}</span></a
		>
	{/if}
	{#if item.price}
		<form method="POST" action="?/approve" use:enhance class="inline">
			<input type="hidden" name="id" value={item.price.id} />
			<button type="submit" class="btn btn-quiet"
				>Approve<span class="visually-hidden"> the price of {item.name}</span></button
			>
		</form>
		<form method="POST" action="?/reject" use:enhance class="inline">
			<input type="hidden" name="id" value={item.price.id} />
			<button type="submit" class="btn btn-quiet danger"
				>Reject<span class="visually-hidden"> the price of {item.name}</span></button
			>
		</form>
	{/if}
	{#if item.step === 'update' && item.proposal_id}
		<form method="POST" action="?/apply" use:enhance class="inline">
			<input type="hidden" name="id" value={item.proposal_id} />
			<button type="submit" class="btn btn-quiet"
				>Apply<span class="visually-hidden"> the changes to {item.name}</span></button
			>
		</form>
	{/if}
	{#if item.proposal_id}
		<form method="POST" action="?/dismiss" use:enhance class="inline">
			<input type="hidden" name="id" value={item.proposal_id} />
			<button type="submit" class="btn btn-quiet"
				>Dismiss<span class="visually-hidden"> {item.name}</span></button
			>
		</form>
	{/if}
{/snippet}

<style>
	.count {
		margin-left: var(--space-1);
		color: var(--text-muted);
		font-weight: 500;
	}

	.provider h2 {
		font-size: var(--text-base, 1rem);
	}

	.provider + .provider {
		margin-top: var(--space-3);
	}

	.stacked {
		display: flex;
		flex-direction: column;
		gap: 2px;
	}
</style>
