<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Props {
		tone?: 'error' | 'warning' | 'info' | 'success';
		title?: string;
		children?: Snippet;
		actions?: Snippet;
	}

	let { tone = 'info', title, children, actions }: Props = $props();
</script>

<div class="notice {tone}" role={tone === 'error' ? 'alert' : 'status'}>
	<div class="text">
		{#if title}<p class="title">{title}</p>{/if}
		{@render children?.()}
	</div>
	{#if actions}<div class="actions">{@render actions()}</div>{/if}
</div>

<style>
	.notice {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-3);
		padding: var(--space-3) var(--space-4);
		border: 1px solid var(--tone);
		border-left-width: 4px;
		border-radius: var(--radius);
		background: var(--tone-soft);
		font-size: var(--text-sm);
	}

	.text {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		min-width: 0;
		overflow-wrap: anywhere;
	}

	.title {
		font-weight: 650;
		color: var(--tone);
	}

	.error {
		--tone: var(--danger);
		--tone-soft: var(--danger-soft);
	}

	.warning {
		--tone: var(--warning);
		--tone-soft: var(--warning-soft);
	}

	.info {
		--tone: var(--info);
		--tone-soft: var(--info-soft);
	}

	.success {
		--tone: var(--success);
		--tone-soft: var(--success-soft);
	}
</style>
