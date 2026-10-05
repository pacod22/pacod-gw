<script lang="ts">
	/** Text from the OpenAPI document: `backticks` become code, nothing else is interpreted. */
	let { text }: { text: string } = $props();

	const parts = $derived(text.split('`').map((value, index) => ({ value, code: index % 2 === 1 })));
</script>

{#each parts as part, index (index)}{#if part.code}<code>{part.value}</code
		>{:else}{part.value}{/if}{/each}
