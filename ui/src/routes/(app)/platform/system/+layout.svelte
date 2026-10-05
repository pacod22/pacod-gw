<script lang="ts">
	import Tabs from '#lib/components/Tabs.svelte';
	import type { LayoutProps } from './$types';

	let { data, children }: LayoutProps = $props();

	const tabs = $derived([
		...(data.principal.platform_admin ? [{ label: 'Status', href: '/platform/system' }] : []),
		{ label: 'Audit log', href: '/platform/system/audit-log' },
		...(data.principal.platform_admin
			? [{ label: 'Configuration', href: '/platform/system/config' }]
			: [])
	]);
</script>

<div class="stack-lg">
	<Tabs label="System" {tabs} />
	{@render children()}
</div>
