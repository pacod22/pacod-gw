<script lang="ts">
	import type { Snippet } from 'svelte';

	let { title, intro, children }: { title: string; intro?: string; children: Snippet } = $props();
</script>

<section>
	<h2>{title}</h2>
	{#if intro}<p class="muted">{intro}</p>{/if}
	{@render children()}
</section>

<style>
	section {
		background: var(--pid-surface);
		border: 1px solid var(--pid-border);
		border-radius: var(--pid-radius-lg);
		padding: var(--pid-space-5);
		margin-bottom: var(--pid-space-5);
		box-shadow: var(--pid-shadow-sm);
	}

	h2 {
		margin: 0 0 var(--pid-space-2);
		font-size: var(--pid-text-lg);
	}

	section > :global(form) {
		max-width: 28rem;
	}
</style>
