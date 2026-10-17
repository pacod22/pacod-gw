<script lang="ts">
	import type { Schemas } from '#lib/api/types.ts';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import { pluginTabs } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	/** A text setting of a binding, for the list. */
	const settingOf = (b: Schemas['PluginBinding'], name: 'engine' | 'catalogue') =>
		String((b.settings as Record<string, unknown> | null)?.[name] ?? '');
	const workspace = $derived(data.workspaces.find((ws) => ws.id === params.ws)?.label ?? params.ws);
</script>

<div class="stack-lg">
	<Tabs label="Plugins" tabs={pluginTabs(params.org, params.ws)} />
	<ResourcePage
		title="Plugins of {workspace}"
		description="Plugins switched on for this workspace. A binding here replaces the organisation’s for the same plugin, unless the organisation locked it; mode off switches the organisation’s off here."
		{data}
		{form}
		columns={[
			{ key: 'plugin', label: 'Plugin', kind: 'code' },
			{ key: 'mode', label: 'Mode' },
			{ key: 'engine', label: 'Engine', value: (b) => settingOf(b, 'engine') || 'llm' },
			{ key: 'catalogue', label: 'Catalogue', value: (b) => settingOf(b, 'catalogue') },
			{ key: 'on_error', label: 'On error' },
			{ key: 'expose', label: 'Intent shown to callers', kind: 'bool' }
		]}
		nameOf={(b) => b.plugin}
		empty="This workspace binds no plugin of its own. Plugins its organisation switches on still apply."
		deleteNote="The organisation’s binding of the plugin, if there is one, applies again."
	/>
</div>
