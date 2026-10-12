<script lang="ts">
	import '@fontsource-variable/inter';
	import '@fontsource-variable/manrope';
	import '@fontsource-variable/jetbrains-mono';
	import '@pacod/ui/tokens.css';
	import '@pacod/ui/console.css';
	import '../app.css';
	import favicon from '#lib/assets/favicon.svg';
	import type { LayoutProps } from './$types';

	let { children }: LayoutProps = $props();
</script>

<svelte:head>
	<link rel="icon" href={favicon} />
</svelte:head>

{@render children()}
