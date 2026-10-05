<script lang="ts">
	import Badge from '#lib/components/Badge.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const organisation = $derived(
		data.orgs.find((org) => org.id === params.org)?.label ?? params.org
	);
	const modelName = (id: string) => data.models[id]?.name ?? id;
</script>

<ResourcePage
	title="Model grants of {organisation}"
	description="The models this organisation may use. Its workspaces can use all of them unless a workspace is restricted to some. Grants are given by platform administrators."
	{data}
	{form}
	nameOf={(grant) => modelName(grant.model_id)}
	columns={[
		{ key: 'model_id', label: 'Model', kind: 'code', value: (g) => modelName(g.model_id) },
		{ key: 'state', label: 'Model state' },
		{ key: 'follow_successor', label: 'Follows the successor', kind: 'bool' },
		{
			key: 'max_output_tokens',
			label: 'Output-token ceiling',
			kind: 'number',
			empty: 'The model’s own'
		},
		{ key: 'created_at', label: 'Granted (UTC)', kind: 'datetime' }
	]}
	empty="This organisation has not been granted any model yet, so its workspaces cannot call the gateway. Add a grant for each model it may use."
	deleteNote="Workspaces of this organisation can no longer use the model. Templates and keys that name it stop working."
	{cell}
	custom={['state']}
/>

{#snippet cell(grant: { model_id: string })}
	{@const state = data.models[grant.model_id]?.state}
	{#if state}<Badge value={state} />{/if}
{/snippet}
