<script lang="ts">
	import type { SchemeStep } from '#lib/routing.ts';
	import Badge from './Badge.svelte';

	/** Routing schemes about to be loaded, each with whether it is created or already there. */
	interface Props {
		steps: SchemeStep[];
	}

	let { steps }: Props = $props();
</script>

<ul class="steps">
	{#each steps as step (step.name)}
		<li>
			<div class="head">
				<code>{step.name}</code>
				{#if step.create}
					<Badge value="will be created" tone="good" />
				{:else}
					<Badge value="exists, kept" tone="neutral" />
				{/if}
			</div>
			{#if step.description}<p class="muted small">{step.description}</p>{/if}
		</li>
	{/each}
</ul>

<style>
	.steps {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		padding: 0;
		list-style: none;
	}

	.head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		gap: var(--space-2);
	}
</style>
