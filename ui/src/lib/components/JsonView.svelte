<script lang="ts">
	import CopyButton from './CopyButton.svelte';

	/** A JSON document shown as it is, to read and to copy: a parameter schema, for example. */
	interface Props {
		value: unknown;
		/** Said to screen readers after "Copy": what is copied. */
		what: string;
	}

	let { value, what }: Props = $props();
	const text = $derived(JSON.stringify(value, null, 2));
</script>

<div class="stack">
	<pre>{text}</pre>
	<div><CopyButton value={text} {what} /></div>
</div>

<style>
	pre {
		max-height: 28rem;
		margin: 0;
		padding: var(--space-3);
		overflow: auto;
		border-radius: var(--radius);
		background: var(--surface-2);
		font-family: var(--font-mono);
		font-size: var(--text-xs);
	}
</style>
