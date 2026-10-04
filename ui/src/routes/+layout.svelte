<script lang="ts">
	import favicon from '#lib/assets/favicon.svg';
	import Nav from '#lib/components/Nav.svelte';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
	<title>pacod-gw</title>
</svelte:head>

<div class="shell">
	<Nav />
	<main>{@render children()}</main>
</div>

<style>
	.shell {
		display: flex;
		min-height: 100vh;
		font-family: system-ui, sans-serif;
	}
	main {
		flex: 1;
		padding: 1.5rem;
	}
</style>
