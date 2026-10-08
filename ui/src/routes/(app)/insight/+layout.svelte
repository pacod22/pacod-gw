<script lang="ts">
	import { page } from '$app/state';
	import Tabs from '#lib/components/Tabs.svelte';
	import { insightTabs, pageScope } from '#lib/nav.ts';
	import type { LayoutProps } from './$types';

	let { data, children }: LayoutProps = $props();

	// The tabs of the section the page belongs to (Usage, Requests, Monitoring), in its scope.
	const section = $derived(
		insightTabs(data.principal, pageScope(page.url.pathname, data.scope), page.url.pathname)
	);
</script>

<div class="stack-lg">
	{#if section}<Tabs label={section.label} tabs={section.tabs} />{/if}
	{@render children()}
</div>
