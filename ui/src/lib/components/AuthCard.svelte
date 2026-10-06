<script lang="ts">
	import type { Snippet } from 'svelte';

	/**
	 * The frame of the pages shown without a session: sign-in, password reset and email
	 * verification. It follows the PACOD sign-in design (pacod-ui, routes/(auth)): the page
	 * surface with one soft brand glow, a rounded card with the monogram, and the form below.
	 */
	interface Props {
		title: string;
		subtitle?: string;
		children: Snippet;
		/** Below the card, for example a link back to the sign-in. */
		footer?: Snippet;
	}

	let { title, subtitle, children, footer }: Props = $props();
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
					<div class="mark" aria-hidden="true">
						<span>GW</span>
						<span class="dot"></span>
					</div>
					<h1>{title}</h1>
					{#if subtitle}<p class="subtitle">{subtitle}</p>{/if}
				</div>

				{@render children()}
			</div>

			{#if footer}<div class="footer">{@render footer()}</div>{/if}
			<p class="wordmark">PACOD · AI Gateway</p>
		</div>
	</main>
</div>

<style>
	.auth {
		position: relative;
		min-height: 100dvh;
		overflow: hidden;
	}

	.glow {
		position: absolute;
		top: 0;
		left: 50%;
		width: 680px;
		height: 460px;
		border-radius: 50%;
		background: var(--accent);
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
		border: 1px solid var(--border);
		border-radius: var(--radius-xl);
		background: var(--surface);
		box-shadow: var(--shadow-sm);
	}

	.head {
		display: flex;
		flex-direction: column;
		align-items: center;
		margin-bottom: 1.75rem;
		text-align: center;
	}

	.mark {
		position: relative;
		display: grid;
		place-items: center;
		width: 44px;
		height: 44px;
		border-radius: 11px;
		background: var(--accent);
		color: #ffffff;
		font-family: var(--font-display);
		font-size: 1rem;
		font-weight: 800;
	}

	.dot {
		position: absolute;
		right: -2px;
		bottom: -2px;
		width: 10px;
		height: 10px;
		border-radius: 50%;
		background: var(--gold);
		box-shadow: 0 0 0 2px var(--surface);
	}

	h1 {
		margin-top: 1.25rem;
		font-size: 1.25rem;
		line-height: 1.4;
		letter-spacing: -0.025em;
	}

	.subtitle {
		margin-top: 0.375rem;
		color: var(--text-muted);
		font-size: 0.875rem;
	}

	.footer {
		margin-top: 1.25rem;
		color: var(--text-muted);
		font-size: 0.875rem;
		text-align: center;
	}

	.wordmark {
		margin-top: 1.5rem;
		color: var(--text-faint);
		font-family: var(--font-mono);
		font-size: 10px;
		letter-spacing: 0.18em;
		text-align: center;
		text-transform: uppercase;
	}

	/* What the pages put in the card. */

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
		border: 1px solid var(--border);
		border-radius: var(--radius);
		background: var(--bg);
		color: var(--text-muted);
		font-size: 0.875rem;
		overflow-wrap: anywhere;
	}

	.card :global(.message.error) {
		border-color: var(--danger);
		background: var(--danger-soft);
		color: var(--danger);
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
		color: var(--text-muted);
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
