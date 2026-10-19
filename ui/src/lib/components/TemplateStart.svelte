<script lang="ts">
	import type { Snippet } from 'svelte';

	/**
	 * The note at the top of a create form that starts from a built-in template, with a switch
	 * between the template and an empty form. Nothing is sent for it.
	 */
	interface Props {
		/** Whether the form shows the template. */
		active: boolean;
		/** What the template is and what to change in it. */
		children: Snippet;
	}

	let { active = $bindable(true), children }: Props = $props();
</script>

<div class="start">
	<p>
		{#if active}
			{@render children()}
		{:else}
			The form starts empty.
		{/if}
	</p>
	<button type="button" class="btn btn-quiet" onclick={() => (active = !active)}>
		{active ? 'Start empty' : 'Use the template'}
	</button>
</div>

<style>
	.start {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-2);
		padding: var(--space-3);
		border: 1px dashed var(--border-strong);
		border-radius: var(--radius);
		background: var(--surface-2);
		color: var(--text-muted);
		font-size: var(--text-sm);
	}

	.start p {
		flex: 1 1 20rem;
	}
</style>
