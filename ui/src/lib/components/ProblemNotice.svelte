<script lang="ts">
	import { refreshAll } from '$app/navigation';
	import type { ApiProblem } from '#lib/api/errors.ts';
	import Notice from './Notice.svelte';

	interface Props {
		problem: ApiProblem;
		/** Called after the data was reloaded, for example to close the form that is stale. */
		onreload?: () => void;
	}

	let { problem, onreload }: Props = $props();

	async function reload() {
		await refreshAll();
		onreload?.();
	}
</script>

<Notice tone={problem.tone} title={problem.title}>
	<p>{problem.message}</p>
	{#each problem.lists as list (list.label)}
		<p><span class="muted">{list.label}:</span> {list.items.join(', ')}</p>
	{/each}
	{#if problem.hint}<p class="muted">{problem.hint}</p>{/if}
	{#snippet actions()}
		{#if problem.reload}
			<button type="button" class="btn" onclick={reload}>Reload the page</button>
		{/if}
	{/snippet}
</Notice>
