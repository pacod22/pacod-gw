<script lang="ts">
	import type { Snippet } from 'svelte';

	/** A message above a form: what happened, or what went wrong. Errors are announced at once. */
	interface Props {
		tone?: 'info' | 'success' | 'warning' | 'error';
		children: Snippet;
	}

	let { tone = 'info', children }: Props = $props();
</script>

<div class="notice {tone}" role={tone === 'error' ? 'alert' : 'status'}>
	{@render children()}
</div>

<style>
	.notice {
		padding: 0.75rem 1rem;
		border: 1px solid var(--pid-border);
		border-radius: var(--pid-radius);
		background: var(--pid-bg);
		color: var(--pid-text-muted);
		font-size: 0.875rem;
		overflow-wrap: anywhere;
	}

	.notice :global(p) {
		margin: 0;
	}

	.notice :global(p + p) {
		margin-top: 0.25rem;
	}

	.success {
		border-color: var(--pid-success);
		background: var(--pid-success-soft);
		color: var(--pid-success);
	}

	.warning {
		border-color: var(--pid-warning);
		background: var(--pid-warning-soft);
		color: var(--pid-warning);
	}

	.error {
		border-color: var(--pid-danger);
		background: var(--pid-danger-soft);
		color: var(--pid-danger);
	}
</style>
