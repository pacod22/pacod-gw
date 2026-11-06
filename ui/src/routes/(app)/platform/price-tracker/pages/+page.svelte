<script lang="ts">
	import { enhance } from '$app/forms';
	import type { Schemas } from '#lib/api/types.ts';
	import ActionDialog from '#lib/components/ActionDialog.svelte';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import type { Column } from '#lib/components/table.ts';
	import { PRICING_TABS } from '#lib/nav.ts';
	import { pageGroups, pageNote } from '#lib/price-pages.ts';
	import type { PageProps } from './$types';

	type Page = Schemas['Item16'];

	let { data, form }: PageProps = $props();
	let adding = $state(false);
	let removing: Page | null = $state(null);

	const groups = $derived(pageGroups(data.pages));
	const waiting = $derived(data.pages.filter((p) => p.status === 'proposed').length);

	const columns: Column<Page>[] = [
		{ key: 'url', label: 'Page', wrap: true },
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'found_by', label: 'Found by', value: (p) => p.found_by.replaceAll('_', ' ') },
		{ key: 'kind', label: 'Kind' },
		{ key: 'priority', label: 'Priority', kind: 'number' },
		{
			key: 'checked',
			label: 'Last checked (UTC)',
			kind: 'datetime',
			value: (p) => p.state?.last_checked_at ?? null,
			empty: 'Not yet'
		},
		{
			key: 'changed',
			label: 'Last changed (UTC)',
			kind: 'datetime',
			value: (p) => p.state?.last_changed_at ?? null,
			empty: '–'
		},
		{ key: 'note', label: 'What the last check found', wrap: true, value: pageNote }
	];
</script>

<div class="stack-lg">
	<Tabs label="Pricing" tabs={PRICING_TABS} />
	<PageHeader
		title="Pricing pages"
		description="The pages the price tracker reads for each provider: pricing pages, model pages and price lists. The weekly scan proposes the pages it finds; a page is read once it is confirmed."
	>
		{#snippet actions()}
			<button type="button" class="btn btn-primary" onclick={() => (adding = true)}
				>Add a page</button
			>
		{/snippet}
	</PageHeader>

	<OutcomeNotice outcome={form} />

	{#if waiting > 0}
		<p class="muted">
			{waiting} page{waiting === 1 ? ' waits' : 's wait'} to be confirmed. Confirm the pages that list
			a provider's models and prices; dismiss the others, and the scan does not propose them again.
		</p>
	{/if}

	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if groups.length === 0}
		<div class="card muted">
			No page yet. The weekly scan proposes the pricing pages of the providers made from a preset;
			ask for a scan on the Price tracker tab, or add a page.
		</div>
	{:else}
		{#each groups as group (group.provider_id)}
			<section class="stack">
				<h2 class="provider">{group.provider}</h2>
				<DataTable
					caption="Pages of {group.provider}"
					{columns}
					rows={group.pages}
					rowKey={(p) => p.id}
					{cell}
					custom={['url']}
					{actions}
				/>
			</section>
		{/each}
	{/if}
</div>

{#snippet cell(page: Page)}
	<a href={page.url} rel="noreferrer noopener" target="_blank">{page.url}</a>
	{#if page.title}<span class="muted small block">{page.title}</span>{/if}
{/snippet}

{#snippet actions(page: Page)}
	{#if page.status !== 'active'}
		<form method="POST" action="?/confirm" use:enhance>
			<input type="hidden" name="id" value={page.id} />
			<input type="hidden" name="url" value={page.url} />
			<button type="submit" class="btn btn-quiet"
				>Confirm<span class="visually-hidden"> {page.url}</span></button
			>
		</form>
	{:else}
		<form method="POST" action="?/check" use:enhance>
			<input type="hidden" name="id" value={page.id} />
			<input type="hidden" name="url" value={page.url} />
			<button type="submit" class="btn btn-quiet" disabled={page.state?.check_requested}
				>Check now<span class="visually-hidden"> {page.url}</span></button
			>
		</form>
	{/if}
	{#if page.status !== 'dismissed'}
		<form method="POST" action="?/dismiss" use:enhance>
			<input type="hidden" name="id" value={page.id} />
			<input type="hidden" name="url" value={page.url} />
			<button type="submit" class="btn btn-quiet"
				>Dismiss<span class="visually-hidden"> {page.url}</span></button
			>
		</form>
	{/if}
	<button type="button" class="btn btn-quiet danger" onclick={() => (removing = page)}
		>Remove<span class="visually-hidden"> {page.url}</span></button
	>
{/snippet}

<ActionDialog
	bind:open={adding}
	title="Add a page"
	action="?/add"
	submit="Add page"
	outcome={form}
	fields={data.fields}
	mode="create"
>
	<p class="muted small">
		The page is read at the next check. A page is rendered in the browser and read by the extraction
		model; a price list is read as it is.
	</p>
</ActionDialog>

<ConfirmDialog
	open={removing !== null}
	title="Remove the page"
	action="?/remove"
	confirm="Remove page"
	outcome={form}
	hidden={{ id: removing?.id, url: removing?.url }}
	onclose={() => (removing = null)}
>
	<p>
		{removing?.url} is no longer read, and what was read from it stays. The scan may propose it again;
		dismiss it instead to keep it out.
	</p>
</ConfirmDialog>

<style>
	.provider {
		font-size: var(--text-lg);
	}

	.block {
		display: block;
	}
</style>
