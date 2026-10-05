<script lang="ts">
	import ObjectPage from '#lib/components/ObjectPage.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<ObjectPage
	title="Settings"
	description="Defaults for the whole installation. Organisations and workspaces can override the ones that are theirs to set."
	{data}
	{form}
/>
