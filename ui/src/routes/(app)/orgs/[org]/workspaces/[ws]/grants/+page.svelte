<script lang="ts">
	import Badge from '#lib/components/Badge.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const workspace = $derived(data.workspaces.find((ws) => ws.id === params.ws)?.label ?? params.ws);
	const modelName = (id: string) => data.models[id]?.name ?? id;
	const unrestricted = $derived(!data.problem && data.items.length === 0 && !data.cursor);
</script>

<ResourcePage
	title="Model grants of {workspace}"
	description="Which of the organisation's models this workspace may use."
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
		}
	]}
	empty="To restrict this workspace to some of the organisation's models, add a grant for each model it may use."
	deleteNote="If this was the workspace's last grant, the workspace may again use every model of its organisation."
	{cell}
	custom={['state']}
>
	{#if unrestricted}
		<Notice tone="info" title="This workspace has no grants of its own">
			<p>
				It may therefore use every model its organisation was granted{data.inherited.length > 0
					? `: ${data.inherited.map((m) => (m.state === 'active' ? m.name : `${m.name} (${m.state})`)).join(', ')}.`
					: '. The organisation has not been granted any model yet.'}
			</p>
		</Notice>
	{:else if !data.problem}
		<Notice tone="info">
			<p>
				This workspace is restricted to the models below. Delete all of its grants to let it use
				every model of its organisation again.
			</p>
		</Notice>
	{/if}
</ResourcePage>

{#snippet cell(grant: { model_id: string })}
	{@const state = data.models[grant.model_id]?.state}
	{#if state}<Badge value={state} />{/if}
{/snippet}
