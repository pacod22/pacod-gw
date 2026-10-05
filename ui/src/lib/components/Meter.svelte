<script lang="ts">
	import { usedShare } from '#lib/limits.ts';

	/**
	 * How much of an allowance is used. The figures are always written out; the bar and its
	 * colour only help the eye: calm while there is room, a warning from 80 %, full at 100 %.
	 */
	interface Props {
		used: number;
		amount: number;
		/** The figures in words: "1,200 tokens of 5,000 tokens". */
		label: string;
	}

	let { used, amount, label }: Props = $props();

	const share = $derived(usedShare(used, amount));
	const percent = $derived(Math.round(share * 100));
	const level = $derived(share >= 1 ? 'full' : share >= 0.8 ? 'near' : 'room');
</script>

<div class="meter">
	<div class="figures">
		<span>{label}</span>
		<span class="muted">{percent}%{level === 'full' ? ' – used up' : ''}</span>
	</div>
	<div
		class="track {level}"
		role="meter"
		aria-valuemin="0"
		aria-valuemax="100"
		aria-valuenow={Math.min(100, percent)}
		aria-label={label}
	>
		<div class="fill" style:width="{Math.min(100, percent)}%"></div>
	</div>
</div>

<style>
	.meter {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		min-width: 14rem;
	}

	.figures {
		display: flex;
		justify-content: space-between;
		gap: var(--space-3);
		font-variant-numeric: tabular-nums;
	}

	/* The track is a lighter step of the fill's colour, so the state reads across the bar. */
	.track {
		--fill: var(--accent);
		--rest: var(--accent-soft);
		height: 0.5rem;
		border-radius: 4px;
		background: var(--rest);
		overflow: hidden;
	}

	.near {
		--fill: var(--warning);
		--rest: var(--warning-soft);
	}

	.full {
		--fill: var(--danger);
		--rest: var(--danger-soft);
	}

	.fill {
		height: 100%;
		border-radius: 4px 0 0 4px;
		background: var(--fill);
	}
</style>
