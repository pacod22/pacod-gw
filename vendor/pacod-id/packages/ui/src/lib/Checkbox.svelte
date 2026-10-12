<script lang="ts">
	import type { HTMLInputAttributes } from 'svelte/elements';

	/** A checkbox with its label beside it, e.g. "Keep me signed in". */
	interface Props extends Omit<HTMLInputAttributes, 'type' | 'checked'> {
		label: string;
		checked?: boolean;
	}

	let { label, checked = $bindable(false), ...rest }: Props = $props();
</script>

<label class="check">
	<input type="checkbox" {...rest} bind:checked />
	<span>{label}</span>
</label>

<style>
	.check {
		display: inline-flex;
		align-items: center;
		gap: 0.5rem;
		color: var(--pid-text-muted);
		font-size: 0.875rem;
		cursor: pointer;
	}

	input {
		width: 1rem;
		height: 1rem;
		margin: 0;
		accent-color: var(--pid-accent);
	}

	input:focus-visible {
		outline: 2px solid var(--pid-focus);
		outline-offset: 2px;
	}
</style>
