<script lang="ts">
	import Tabs from '#lib/components/Tabs.svelte';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();

	const tabs = [
		{ label: 'Run', href: '/platform/testing' },
		{ label: 'Message', href: '/platform/testing/message' },
		{ label: 'History', href: '/platform/testing/history' },
		{ label: 'Cases', href: '/platform/testing/cases' }
	];
</script>

<div class="stack-lg">
	<Tabs label="Testing" {tabs} />
	{@render children()}
</div>
