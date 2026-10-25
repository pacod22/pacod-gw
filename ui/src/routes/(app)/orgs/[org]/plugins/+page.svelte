<script lang="ts">
	import { intentOnly, platformBound, settingsSummary } from '#lib/plugins.ts';
	import BindingTemplateIntro from '#lib/components/BindingTemplateIntro.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import ResponseCacheCard from '#lib/components/ResponseCacheCard.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import { pluginTabs, scopeQuery } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();
	// The form to add a binding starts from the core's template, unless asked to start empty.
	let fromTemplate = $state(true);

	const platform = $derived(platformBound(data.plugins));
</script>

<div class="stack-lg">
	<Tabs label="Plugins" tabs={pluginTabs(params.org)} />
	{#if platform.length}
		<p class="muted">
			The platform binds {platform.join(', ')} for every workspace that binds it otherwise not. A binding
			here replaces it; mode off switches it off.
		</p>
	{/if}
	<ResourcePage
		title="Plugins"
		description="Plugins this organisation switches on for all its workspaces. A workspace may bind a plugin of its own instead, unless the binding here is locked. Start in shadow mode: it records what it would do and changes no call."
		{data}
		{form}
		columns={[
			{ key: 'plugin', label: 'Plugin', kind: 'code' },
			{ key: 'mode', label: 'Mode' },
			{ key: 'settings', label: 'Settings', value: settingsSummary },
			{ key: 'on_error', label: 'On error', value: (b) => intentOnly(b, b.on_error ?? 'closed') },
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

	{#if data.cache}
		<ResponseCacheCard
			cache={data.cache}
			scope="organisation"
			canClear={data.canClearCache}
			statsHref="/insight/cache{scopeQuery({ org: params.org, ws: null })}"
			outcome={form}
		/>
	{/if}
</div>
