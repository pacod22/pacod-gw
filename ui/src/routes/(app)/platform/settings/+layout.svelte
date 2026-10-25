<script lang="ts">
	import Tabs from '#lib/components/Tabs.svelte';
	import { SETTINGS_TABS } from '#lib/platform-settings.ts';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
</script>

<div class="stack-lg">
	<Tabs label="Platform settings" tabs={SETTINGS_TABS} />
	{@render children()}
</div>
