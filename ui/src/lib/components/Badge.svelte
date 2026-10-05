<script lang="ts">
	/** A state in a word. The tone follows the word unless it is given. */
	interface Props {
		value: string;
		tone?: 'good' | 'bad' | 'warn' | 'neutral';
	}

	let { value, tone }: Props = $props();

	const TONES: Record<string, NonNullable<Props['tone']>> = {
		active: 'good',
		draft: 'neutral',
		deprecated: 'warn',
		retired: 'bad',
		proposed: 'warn',
		superseded: 'neutral',
		advisory: 'neutral',
		current: 'good',
		ok: 'good',
		disabled: 'neutral',
		not_configured: 'neutral',
		cancelled: 'neutral',
		retiring: 'warn',
		timeout: 'warn',
		rejected: 'warn',
		revoked: 'bad',
		down: 'bad',
		error: 'bad'
	};

	const resolved = $derived(tone ?? TONES[value] ?? 'neutral');
</script>

<span class="badge {resolved}">{value.replaceAll('_', ' ')}</span>

<style>
	.badge {
		display: inline-block;
		padding: 0.05rem var(--space-2);
		border: 1px solid var(--tone);
		border-radius: 999px;
		background: var(--tone-soft);
		color: var(--tone);
		font-size: var(--text-xs);
		font-weight: 650;
		line-height: 1.5;
		white-space: nowrap;
	}

	.good {
		--tone: var(--success);
		--tone-soft: var(--success-soft);
	}

	.bad {
		--tone: var(--danger);
		--tone-soft: var(--danger-soft);
	}

	.warn {
		--tone: var(--warning);
		--tone-soft: var(--warning-soft);
	}

	.neutral {
		--tone: var(--text-muted);
		--tone-soft: var(--surface-2);
	}
</style>
