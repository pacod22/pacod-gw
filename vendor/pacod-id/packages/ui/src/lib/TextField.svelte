<script lang="ts">
	import type { HTMLInputAttributes } from 'svelte/elements';
	import './field.css';

	/** A labelled text input, with an optional hint and error below it. */
	interface Props extends Omit<HTMLInputAttributes, 'value'> {
		label: string;
		value?: string;
		hint?: string;
		/** Shown instead of the hint, and marks the input invalid. */
		error?: string;
	}

	let { label, value = $bindable(''), hint, error, id, ...rest }: Props = $props();
	const uid = $props.id();
	const inputId = $derived(id ?? `pid-${uid}`);
</script>

<div class="pid-field">
	<label for={inputId}>{label}</label>
	<input
		id={inputId}
		type="text"
		aria-invalid={error ? 'true' : undefined}
		aria-describedby={error || hint ? `${inputId}-note` : undefined}
		{...rest}
		bind:value
	/>
	{#if error}
		<p class="pid-error" id="{inputId}-note">{error}</p>
	{:else if hint}
		<p class="pid-hint" id="{inputId}-note">{hint}</p>
	{/if}
</div>
