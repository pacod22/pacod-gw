<script lang="ts">
	import type { HTMLInputAttributes } from 'svelte/elements';
	import './field.css';

	/**
	 * A password input with a show/hide toggle. With `minLength`, a live hint counts the
	 * characters still missing; the strings come from the page, which knows its language.
	 */
	interface Props extends Omit<HTMLInputAttributes, 'value' | 'type'> {
		label: string;
		value?: string;
		error?: string;
		hint?: string;
		/** The password policy's minimum, for the live hint. */
		minLength?: number;
		/** The live hint while the password is too short, e.g. (n) => `${n} more characters`. */
		missing?: (n: number) => string;
		showLabel?: string;
		hideLabel?: string;
	}

	let {
		label,
		value = $bindable(''),
		error,
		hint,
		minLength,
		missing,
		showLabel = 'Show',
		hideLabel = 'Hide',
		id,
		...rest
	}: Props = $props();
	const uid = $props.id();
	const inputId = $derived(id ?? `pid-${uid}`);
	let visible = $state(false);
	const short = $derived(
		minLength !== undefined && value.length > 0 ? minLength - [...value].length : 0
	);
	const note = $derived(error ?? (short > 0 && missing ? missing(short) : hint));
</script>

<div class="pid-field">
	<label for={inputId}>{label}</label>
	<div class="control">
		<input
			id={inputId}
			type={visible ? 'text' : 'password'}
			minlength={minLength}
			aria-invalid={error ? 'true' : undefined}
			aria-describedby={note ? `${inputId}-note` : undefined}
			{...rest}
			bind:value
		/>
		<button
			type="button"
			class="toggle"
			aria-controls={inputId}
			aria-pressed={visible}
			onclick={() => (visible = !visible)}
		>
			{visible ? hideLabel : showLabel}
		</button>
	</div>
	{#if note}
		<p class={error ? 'pid-error' : 'pid-hint'} id="{inputId}-note" aria-live="polite">{note}</p>
	{/if}
</div>

<style>
	.control {
		position: relative;
	}

	.control input {
		padding-right: 4.5rem;
	}

	.toggle {
		position: absolute;
		top: 50%;
		right: 0.375rem;
		padding: 0.25rem 0.625rem;
		border: 0;
		border-radius: 999px;
		background: transparent;
		color: var(--pid-text-muted);
		font-family: var(--pid-font-display);
		font-size: var(--pid-text-xs);
		font-weight: 600;
		cursor: pointer;
		transform: translateY(-50%);
	}

	.toggle:hover {
		background: var(--pid-surface-2);
		color: var(--pid-text);
	}

	.toggle:focus-visible {
		outline: 2px solid var(--pid-focus);
		outline-offset: 1px;
	}
</style>
