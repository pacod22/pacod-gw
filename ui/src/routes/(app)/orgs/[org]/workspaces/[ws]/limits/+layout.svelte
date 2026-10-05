<script lang="ts">
	import Tabs from '#lib/components/Tabs.svelte';
	import { workspacePath } from '#lib/nav.ts';
	import type { LayoutProps } from './$types';

	let { children, params }: LayoutProps = $props();
</script>

<div class="stack-lg">
	<Tabs
		label="Limits"
		tabs={[
			{ label: 'Limits', href: workspacePath(params.org, params.ws, 'limits') },
			{
				label: 'End-user overrides',
				href: workspacePath(params.org, params.ws, 'limits/overrides')
			}
		]}
	/>
	{@render children()}
</div>
