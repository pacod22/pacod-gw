<script lang="ts">
	import type { Schemas } from '#lib/api/types.ts';
	import BindingTemplateIntro from '#lib/components/BindingTemplateIntro.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import { pluginTabs } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();
	// The form to add a binding starts from the core's template, unless asked to start empty.
	let fromTemplate = $state(true);

	/** A text setting of a binding, for the list. */
	const settingOf = (b: Schemas['PluginBinding'], name: 'engine' | 'catalogue') =>
		String((b.settings as Record<string, unknown> | null)?.[name] ?? '');
</script>

<div class="stack-lg">
	<Tabs label="Plugins" tabs={pluginTabs(params.org)} />
	<ResourcePage
		title="Plugins"
		description="Plugins this organisation switches on for all its workspaces. A workspace may bind a plugin of its own instead, unless the binding here is locked. Start in shadow mode: it records what it would do and changes no call."
		{data}
		{form}
		columns={[
			{ key: 'plugin', label: 'Plugin', kind: 'code' },
			{ key: 'mode', label: 'Mode' },
			{ key: 'engine', label: 'Engine', value: (b) => settingOf(b, 'engine') || 'llm' },
			{ key: 'catalogue', label: 'Catalogue', value: (b) => settingOf(b, 'catalogue') },
			{ key: 'on_error', label: 'On error' },
			{ key: 'locked', label: 'Locked', kind: 'bool' }
		]}
		nameOf={(b) => b.plugin}
		empty="No plugin is switched on for this organisation. Add a binding to switch one on for all its workspaces."
		deleteNote="The plugin no longer looks at the calls of the workspaces that follow this binding."
		createDefaults={fromTemplate ? (data.bindingTemplate?.binding ?? null) : null}
	>
		{#snippet formIntro(mode)}
			{#if mode === 'create' && data.bindingTemplate}
				<BindingTemplateIntro template={data.bindingTemplate} bind:active={fromTemplate} />
			{/if}
		{/snippet}
	</ResourcePage>
</div>
