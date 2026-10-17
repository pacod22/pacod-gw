<script lang="ts">
	import type { Snippet } from 'svelte';
	import BrandMark from './BrandMark.svelte';
	import { BRANDS, type Brand } from './brands.js';

	/**
	 * The frame of the pages shown without a session: sign-in, sign-up, password reset, email
	 * verification, invitations. The design: the page surface with one soft brand
	 * glow, a rounded card with the brand mark, and the form below.
	 */
	interface Props {
		title: string;
		subtitle?: string;
		brand?: Brand;
		/** Overrides the brand's monogram in the mark. */
		monogram?: string;
		/** The line below the card; the brand's wordmark by default, '' for none. */
		wordmark?: string;
		children: Snippet;
		/** Below the card, for example a link back to the sign-in. */
		footer?: Snippet;
	}

	let {
		title,
		subtitle,
		brand = 'gateway',
		monogram,
		wordmark,
		children,
		footer
	}: Props = $props();
	const mark = $derived(wordmark ?? BRANDS[brand].wordmark);
</script>

<svelte:head>
	<!-- The links of the account emails carry one-time tokens: nothing here is to be indexed. -->
	<meta name="robots" content="noindex, nofollow" />
</svelte:head>

<div class="auth">
	<div class="glow" aria-hidden="true"></div>

	<main>
		<div class="column">
			<div class="card">
				<div class="head">
					<BrandMark {brand} {monogram} />
					<h1>{title}</h1>
					{#if subtitle}<p class="subtitle">{subtitle}</p>{/if}
				</div>

				{@render children()}
			</div>

			{#if footer}<div class="footer">{@render footer()}</div>{/if}
			{#if mark}<p class="wordmark">{mark}</p>{/if}
		</div>
	</main>
</div>

<style>
	.auth {
		position: relative;
		min-height: 100dvh;
		overflow: hidden;
		background: var(--pid-bg);
		color: var(--pid-text);
		font-family: var(--pid-font);
	}

	.glow {
		position: absolute;
		top: 0;
		left: 50%;
		width: 680px;
		height: 460px;
		border-radius: 50%;
		background: var(--pid-accent);
		opacity: 0.1;
		filter: blur(64px);
		transform: translate(-50%, -33.333%);
		pointer-events: none;
	}

	main {
		position: relative;
		display: grid;
		place-items: center;
		min-height: 100dvh;
		padding: 1rem;
	}

	.column {
		width: 100%;
		max-width: 400px;
	}

	.card {
		padding: 1.5rem;
		border: 1px solid var(--pid-border);
		border-radius: var(--pid-radius-xl);
		background: var(--pid-surface);
		box-shadow: var(--pid-shadow-sm);
	}

	.head {
		display: flex;
		flex-direction: column;
		align-items: center;
		margin-bottom: 1.75rem;
		text-align: center;
	}

	h1 {
		margin: 1.25rem 0 0;
		font-family: var(--pid-font-display);
		font-size: 1.25rem;
		font-weight: 700;
		line-height: 1.4;
		letter-spacing: -0.025em;
	}

	.subtitle {
		margin: 0.375rem 0 0;
		color: var(--pid-text-muted);
		font-size: 0.875rem;
	}

	.footer {
		margin-top: 1.25rem;
		color: var(--pid-text-muted);
		font-size: 0.875rem;
		text-align: center;
	}

	.wordmark {
		margin: 1.5rem 0 0;
		color: var(--pid-text-faint);
		font-family: var(--pid-font-mono);
		font-size: 10px;
		letter-spacing: 0.18em;
		text-align: center;
		text-transform: uppercase;
	}

	/* What pages put in the card, when they use their own markup instead of the components. */

	.card :global(form) {
		display: flex;
		flex-direction: column;
		gap: 1rem;
	}

	.card :global(.field) {
		display: flex;
		flex-direction: column;
		gap: 0.375rem;
	}

	.card :global(.btn-wide) {
		width: 100%;
		height: 2.5rem;
		margin-top: 0.25rem;
		font-size: 0.875rem;
	}

	.card :global(.message) {
		margin-bottom: 1.25rem;
		padding: 0.75rem 1rem;
		border: 1px solid var(--pid-border);
		border-radius: var(--pid-radius);
		background: var(--pid-bg);
		color: var(--pid-text-muted);
		font-size: 0.875rem;
		overflow-wrap: anywhere;
	}

	.card :global(.message.error) {
		border-color: var(--pid-danger);
		background: var(--pid-danger-soft);
		color: var(--pid-danger);
	}

	.card :global(.message .hint) {
		margin-top: 0.25rem;
		opacity: 0.85;
	}

	.card :global(.message form) {
		margin-top: 0.5rem;
	}

	.card :global(.link-button) {
		padding: 0;
		border: 0;
		background: none;
		color: inherit;
		font: inherit;
		font-weight: 600;
		text-decoration: underline;
		cursor: pointer;
	}

	.card :global(.options) {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: 0.5rem;
		font-size: 0.875rem;
	}

	.card :global(.options label) {
		display: flex;
		align-items: center;
		gap: 0.5rem;
		color: var(--pid-text-muted);
		cursor: pointer;
	}

	.card :global(.options a) {
		font-weight: 600;
	}

	@media (min-width: 640px) {
		main {
			padding: 1.5rem;
		}

		.card {
			padding: 2rem;
		}
	}
</style>
