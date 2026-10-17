<script lang="ts">
	import { BRANDS, type Brand } from './brands.js';

	/** The rounded square with the brand's monogram and the gold dot. */
	interface Props {
		brand?: Brand;
		/** Overrides the brand's monogram. */
		monogram?: string;
		/** Edge length in pixels. */
		size?: number;
	}

	let { brand = 'gateway', monogram, size = 44 }: Props = $props();
	const text = $derived(monogram ?? BRANDS[brand].monogram);
</script>

<div class="mark" style:--size="{size}px" aria-hidden="true">
	<span>{text}</span>
	<span class="dot"></span>
</div>

<style>
	.mark {
		position: relative;
		display: grid;
		flex: none;
		place-items: center;
		width: var(--size);
		height: var(--size);
		border-radius: calc(var(--size) / 4);
		background: var(--pid-accent);
		/* White in both schemes. */
		color: #ffffff;
		font-family: var(--pid-font-display);
		/* 16px at the standard 44px. */
		font-size: calc(var(--size) / 44 * 16);
		font-weight: 800;
	}

	.dot {
		position: absolute;
		right: -2px;
		bottom: -2px;
		width: calc(var(--size) / 44 * 10);
		height: calc(var(--size) / 44 * 10);
		border-radius: 50%;
		background: var(--pid-gold);
		box-shadow: 0 0 0 2px var(--pid-surface);
	}
</style>
