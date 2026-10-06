<script lang="ts">
	import Tabs from '#lib/components/Tabs.svelte';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
</script>

<div class="stack-lg">
	<Tabs
		label="Access"
		tabs={[
			{ label: 'Users', href: '/platform/access/users' },
			{ label: 'Admin tokens', href: '/platform/access/admin-tokens' }
		]}
	/>
	{@render children()}
</div>
