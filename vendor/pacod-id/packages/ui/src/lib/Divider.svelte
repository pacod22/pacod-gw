<script lang="ts">
	/** A rule with a word in the middle, between two ways to sign in ("or"). */
	interface Props {
		label?: string;
	}

	let { label }: Props = $props();
</script>

<div class="divider" role="separator">
	{#if label}<span>{label}</span>{/if}
</div>

<style>
	.divider {
		display: flex;
		align-items: center;
		gap: 0.75rem;
		color: var(--pid-text-faint);
		font-family: var(--pid-font-mono);
		font-size: 0.6875rem;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}

	.divider::before,
	.divider::after {
		content: '';
		flex: 1;
		height: 1px;
		background: var(--pid-border);
	}
</style>
