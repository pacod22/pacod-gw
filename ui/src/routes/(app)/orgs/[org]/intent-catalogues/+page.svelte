<script lang="ts">
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import TemplateStart from '#lib/components/TemplateStart.svelte';
	import { pluginTabs } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();
	// The form to add a catalogue starts from the built-in template, unless asked to start empty.
	let fromTemplate = $state(true);
</script>

<div class="stack-lg">
	<Tabs label="Plugins" tabs={pluginTabs(params.org)} />
	<ResourcePage
		title="Intent catalogues"
		description="Intents to tell calls apart by, the further questions to ask about them, and the rules that act on the answers. Bindings of this organisation and of its workspaces name a catalogue."
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
		createDefaults={fromTemplate ? data.template : null}
	>
		{#snippet formIntro(mode)}
			{#if mode === 'create' && data.template}
				<TemplateStart bind:active={fromTemplate}>
					The form starts from the built-in template: a general assistant&rsquo;s intents, questions
					of each kind, and rules that flag and block. Change it to what your application sees, and
					give it a name of its own; a catalogue&rsquo;s name cannot change once it is created.
				</TemplateStart>
			{/if}
		{/snippet}
	</ResourcePage>
</div>
