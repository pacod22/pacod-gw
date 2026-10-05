<script lang="ts">
	import type { Schemas } from '#lib/api/types.ts';
	import DataTable from '#lib/components/DataTable.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import { draftModelPath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	type Discovered = Schemas['Discovered'];

	let { data }: PageProps = $props();

	const provider = $derived(data.provider?.name ?? 'provider');
	const candidates = $derived((data.discovery?.listed ?? []).filter((m) => !m.model).length);

	/** The model form with what the provider told about the model filled in. */
	const draft = (model: Discovered) =>
		draftModelPath({ id: data.provider?.id ?? '', name: provider }, model);
</script>

<div class="stack-lg">
	<PageHeader
		title="Models {provider} offers"
		description="What the provider lists right now, against the model repository. A model the repository lacks can be added as a draft with the provider's details filled in."
		crumbs={[{ label: 'Providers', href: '/platform/providers' }]}
	/>
	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if data.discovery}
		{#if data.discovery.missing.length > 0}
			<Notice tone="warning" title="The provider no longer lists models of the repository">
				<p>{data.discovery.missing.join(', ')}</p>
			</Notice>
		{/if}
		<p class="muted">
			{data.discovery.listed.length} listed, {candidates} of them not in the repository.
		</p>
		{#if data.discovery.listed.length === 0}
			<div class="card muted">The provider lists no models.</div>
		{:else}
			<DataTable
				caption="Models the provider lists"
				rows={data.discovery.listed}
				rowKey={(m) => m.upstream_id}
				columns={[
					{ key: 'upstream_id', label: 'Provider’s id', kind: 'code' },
					{
						key: 'model',
						label: 'In the repository as',
						kind: 'code',
						empty: 'Not in the repository'
					},
					{ key: 'display_name', label: 'Name' },
					{ key: 'context_window', label: 'Context window', kind: 'number' },
					{ key: 'max_output_tokens', label: 'Output-token limit', kind: 'number' }
				]}
				actions={data.mayCreate ? create : undefined}
			/>
		{/if}
	{/if}
</div>

{#snippet create(model: Discovered)}
	{#if !model.model}
		<a class="btn btn-quiet" href={draft(model)}>
			Create draft<span class="visually-hidden"> for {model.upstream_id}</span>
		</a>
	{/if}
{/snippet}
