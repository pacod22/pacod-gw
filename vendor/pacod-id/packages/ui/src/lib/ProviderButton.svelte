<script lang="ts">
	import type { HTMLButtonAttributes } from 'svelte/elements';

	/** "Continue with Google": the provider's mark and a label in the page's language. */
	interface Props extends HTMLButtonAttributes {
		provider: 'google';
		label: string;
	}

	let { provider, label, ...rest }: Props = $props();
</script>

<button type="submit" class="provider" data-provider={provider} {...rest}>
	{#if provider === 'google'}
		<svg viewBox="0 0 18 18" width="18" height="18" aria-hidden="true">
			<path
				fill="#4285F4"
				d="M17.64 9.2c0-.64-.06-1.25-.16-1.84H9v3.48h4.84a4.14 4.14 0 0 1-1.8 2.71v2.26h2.92c1.7-1.57 2.68-3.88 2.68-6.61z"
			/>
			<path
				fill="#34A853"
				d="M9 18c2.43 0 4.47-.8 5.96-2.18l-2.92-2.26c-.8.54-1.84.86-3.04.86-2.34 0-4.32-1.58-5.03-3.71H.96v2.33A9 9 0 0 0 9 18z"
			/>
			<path
				fill="#FBBC05"
				d="M3.97 10.71A5.41 5.41 0 0 1 3.68 9c0-.6.1-1.17.29-1.71V4.96H.96A9 9 0 0 0 0 9c0 1.45.35 2.83.96 4.04l3.01-2.33z"
			/>
			<path
				fill="#EA4335"
				d="M9 3.58c1.32 0 2.5.45 3.44 1.35l2.58-2.58A8.65 8.65 0 0 0 9 0 9 9 0 0 0 .96 4.96l3.01 2.33C4.68 5.16 6.66 3.58 9 3.58z"
			/>
		</svg>
	{/if}
	<span>{label}</span>
</button>

<style>
	.provider {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		gap: 0.625rem;
		width: 100%;
		height: var(--pid-control-height);
		border: 1px solid var(--pid-border-strong);
		border-radius: 999px;
		background: var(--pid-surface);
		color: var(--pid-text);
		font-family: var(--pid-font-display);
		font-size: 0.875rem;
		font-weight: 600;
		cursor: pointer;
		transition:
			background-color 0.15s,
			border-color 0.15s;
	}

	.provider:hover {
		border-color: var(--pid-text);
		background: var(--pid-surface-2);
	}

	.provider:focus-visible {
		outline: 2px solid var(--pid-focus);
		outline-offset: 2px;
	}

	.provider:disabled {
		opacity: 0.45;
		cursor: not-allowed;
	}
</style>
