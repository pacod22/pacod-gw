<script lang="ts">
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import { pluginTabs } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();
</script>

<div class="stack-lg">
	<Tabs label="Plugins" tabs={pluginTabs(params.org, params.ws)} />
	<ResourcePage
		title="Intent catalogues of this workspace"
		description="A workspace's own catalogues. One with the name of an organisation's catalogue takes its place for this workspace."
		{data}
		{form}
		columns={[
			{ key: 'name', label: 'Name', kind: 'code' },
			{ key: 'description', label: 'Description' },
			{ key: 'intents', label: 'Intents', value: (c) => c.intents.map((i) => i.name).join(', ') },
			{
				key: 'questions',
				label: 'Questions',
				value: (c) => (c.questions ?? []).map((q) => q.id).join(', '),
				empty: '–'
			},
			{ key: 'rules', label: 'Rules', kind: 'number', value: (c) => (c.rules ?? []).length }
		]}
		empty="No intent catalogue yet. Add one, then name it in a binding of the intent plugin."
		deleteNote="A catalogue that plugin bindings name cannot be deleted; change the bindings first."
	/>
</div>
