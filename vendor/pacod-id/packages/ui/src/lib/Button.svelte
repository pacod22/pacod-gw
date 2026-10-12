<script lang="ts">
	import type { Snippet } from 'svelte';

	/** A pill in the display face. With `href` it is a link that looks like a button. */
	interface Props {
		variant?: 'primary' | 'secondary' | 'quiet' | 'danger';
		/** The full width of its container, at the height of a form control. */
		wide?: boolean;
		/** Disables the button and tells assistive technology that something is under way. */
		busy?: boolean;
		/** Makes it a link. */
		href?: string;
		type?: 'button' | 'submit' | 'reset';
		disabled?: boolean;
		name?: string;
		value?: string;
		form?: string;
		formaction?: string;
		title?: string;
		'aria-label'?: string;
		onclick?: (event: MouseEvent) => void;
		children: Snippet;
	}

	let {
		variant = 'secondary',
		wide = false,
		busy = false,
		href,
		type = 'button',
		disabled = false,
		children,
		...rest
	}: Props = $props();
</script>

{#if href !== undefined}
	<a class="btn {variant}" class:wide {href} title={rest.title} aria-label={rest['aria-label']}
		>{@render children()}</a
	>
{:else}
	<button
		class="btn {variant}"
		class:wide
		{type}
		disabled={disabled || busy}
		aria-busy={busy || undefined}
		{...rest}
	>
		{@render children()}
	</button>
{/if}

<style>
	.btn {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: var(--pid-space-2);
		min-height: 2.25rem;
		padding: 0 var(--pid-space-4);
		border: 1px solid var(--pid-border-strong);
		border-radius: 999px;
		background: transparent;
		color: var(--pid-text);
		font-family: var(--pid-font-display);
		font-size: var(--pid-text-sm);
		font-weight: 600;
		letter-spacing: -0.01em;
		line-height: 1.2;
		white-space: nowrap;
		cursor: pointer;
		text-decoration: none;
		transition:
			background-color 0.15s,
			border-color 0.15s,
			color 0.15s;
	}

	.btn:hover {
		border-color: var(--pid-text);
		color: var(--pid-text);
		text-decoration: none;
	}

	.btn:active:not(:disabled) {
		transform: translateY(1px);
	}

	.btn:focus-visible {
		outline: 2px solid var(--pid-focus);
		outline-offset: 2px;
	}

	.btn:disabled {
		opacity: 0.45;
		cursor: not-allowed;
	}

	.primary {
		border-color: var(--pid-accent);
		background: var(--pid-accent);
		color: var(--pid-on-accent);
		box-shadow: 0 1px 2px rgb(0 0 0 / 0.06);
	}

	.primary:hover {
		border-color: var(--pid-accent-hover);
		background: var(--pid-accent-hover);
		color: var(--pid-on-accent);
	}

	.danger {
		border-color: var(--pid-danger);
		background: var(--pid-danger);
		color: var(--pid-on-danger);
	}

	.danger:hover {
		border-color: var(--pid-danger-hover);
		background: var(--pid-danger-hover);
		color: var(--pid-on-danger);
	}

	.quiet {
		min-height: 1.875rem;
		padding: 0 var(--pid-space-3);
		border-color: transparent;
		color: var(--pid-text-muted);
	}

	.quiet:hover {
		border-color: transparent;
		background: var(--pid-surface-2);
		color: var(--pid-text);
	}

	.wide {
		width: 100%;
		height: var(--pid-control-height);
		font-size: 0.875rem;
	}
</style>
