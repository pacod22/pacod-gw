<script lang="ts">
	import Tabs from '#lib/components/Tabs.svelte';
	import type { Tab } from '#lib/nav.ts';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();

	const tabs: Tab[] = [
		{ label: 'Run', href: '/platform/testing' },
		{ label: 'Message', href: '/platform/testing/message', match: '/platform/testing/message' },
		{
			label: 'History',
			href: '/platform/testing/history',
			// A run opened from the history.
			match: ['/platform/testing/history', '/platform/testing/runs']
		},
		{ label: 'Cases', href: '/platform/testing/cases' }
	];
</script>

<div class="stack-lg">
	<Tabs label="Testing" {tabs} />
	{@render children()}
</div>
