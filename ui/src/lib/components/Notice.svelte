<script lang="ts">
	import type { Snippet } from 'svelte';
	import CircleAlert from '@lucide/svelte/icons/circle-alert';
	import CircleCheck from '@lucide/svelte/icons/circle-check';
	import Info from '@lucide/svelte/icons/info';
	import TriangleAlert from '@lucide/svelte/icons/triangle-alert';

	interface Props {
		tone?: 'error' | 'warning' | 'info' | 'success';
		title?: string;
		children?: Snippet;
		actions?: Snippet;
	}

	let { tone = 'info', title, children, actions }: Props = $props();

	const ICONS = { error: CircleAlert, warning: TriangleAlert, info: Info, success: CircleCheck };
	const Icon = $derived(ICONS[tone]);
</script>

<div class="notice {tone}" role={tone === 'error' ? 'alert' : 'status'}>
	<span class="icon" aria-hidden="true"><Icon size={16} /></span>
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
		align-items: flex-start;
		gap: var(--space-2) var(--space-3);
		padding: var(--space-3) var(--space-4);
		border: 1px solid color-mix(in srgb, var(--tone) 55%, transparent);
		border-radius: var(--radius-lg);
		background: color-mix(in srgb, var(--tone-soft) 70%, var(--surface));
		font-size: var(--text-sm);
	}

	.icon {
		display: inline-flex;
		flex: none;
		margin-top: 2px;
		color: var(--tone);
	}

	.text {
		display: flex;
		flex: 1 1 16rem;
		flex-direction: column;
		gap: var(--space-1);
		min-width: 0;
		overflow-wrap: anywhere;
	}

	.title {
		font-weight: 650;
		color: var(--tone);
	}

	.actions {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
		align-self: center;
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
