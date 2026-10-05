<script lang="ts">
	import { onMount } from 'svelte';
	import { afterNavigate } from '$app/navigation';
	import { page } from '$app/state';
	import { describeRole } from '#lib/access.ts';
	import ScopeSwitcher from '#lib/components/ScopeSwitcher.svelte';
	import { startLiveUpdates, type LiveState } from '#lib/live.ts';
	import { buildNav, isCurrent } from '#lib/nav.ts';
	import type { LayoutProps } from './$types';

	let { data, children }: LayoutProps = $props();

	const groups = $derived(buildNav(data.principal, data.scope));
	const roles = $derived(data.principal.roles.map(describeRole));

	let menuOpen = $state(false);
	let live: LiveState = $state('connecting');

	afterNavigate(() => {
		menuOpen = false;
	});

	onMount(() =>
		startLiveUpdates({
			onstate: (state) => {
				live = state;
			}
		})
	);

	const LIVE_TEXT: Record<LiveState, string> = {
		connecting: 'Connecting to live updates…',
		live: 'Live updates are on',
		offline: 'Live updates are off. Reload to reconnect.'
	};
</script>

<a class="skip" href="#main">Skip to the content</a>

<div class="shell">
	<aside class="sidebar">
		<div class="brand">
			<a href="/" class="name">pacod-gw</a>
			<button
				type="button"
				class="btn menu"
				aria-expanded={menuOpen}
				aria-controls="sidebar-body"
				onclick={() => (menuOpen = !menuOpen)}
			>
				{menuOpen ? 'Close menu' : 'Menu'}
			</button>
		</div>

		<div id="sidebar-body" class="body" class:open={menuOpen}>
			<ScopeSwitcher scope={data.scope} orgs={data.orgs} workspaces={data.workspaces} />

			<nav aria-label="Main">
				{#each groups as group (group.label)}
					<section>
						<h2>{group.label}</h2>
						{#if group.hint}
							<p class="hint">{group.hint}</p>
						{:else}
							<ul>
								{#each group.items as item (item.href)}
									<li>
										<a
											href={item.href}
											aria-current={isCurrent(item, page.url.pathname) ? 'page' : undefined}
										>
											{item.label}
										</a>
									</li>
								{/each}
							</ul>
						{/if}
					</section>
				{/each}
			</nav>

			<div class="account">
				<p class="who" title={data.principal.id}>{data.principal.display}</p>
				<p class="hint">
					{roles[0] ?? 'No role yet'}{#if roles.length > 1}, and {roles.length - 1} more{/if}
				</p>
				<div class="account-actions">
					<a href="/account/password">Change password</a>
					<form method="POST" action="/logout">
						<button type="submit" class="btn">Sign out</button>
					</form>
				</div>
				<p class="live {live}" role="status">{LIVE_TEXT[live]}</p>
			</div>
		</div>
	</aside>

	<main id="main" tabindex="-1">
		{@render children()}
	</main>
</div>

<style>
	.skip {
		position: absolute;
		left: var(--space-2);
		top: -4rem;
		z-index: 10;
		padding: var(--space-2) var(--space-3);
		border-radius: var(--radius);
		background: var(--accent);
		color: var(--on-accent);
	}

	.skip:focus {
		top: var(--space-2);
	}

	.shell {
		display: grid;
		grid-template-columns: var(--sidebar-width) minmax(0, 1fr);
		min-height: 100dvh;
	}

	.sidebar {
		position: sticky;
		top: 0;
		align-self: start;
		display: flex;
		flex-direction: column;
		height: 100dvh;
		overflow-y: auto;
		border-right: 1px solid var(--border);
		background: var(--surface);
	}

	.brand {
		display: flex;
		align-items: center;
		justify-content: space-between;
		padding: var(--space-4);
	}

	.name {
		color: var(--text);
		font-size: var(--text-lg);
		font-weight: 700;
		letter-spacing: -0.01em;
	}

	.menu {
		display: none;
	}

	.body {
		display: flex;
		flex: 1;
		flex-direction: column;
		gap: var(--space-5);
		padding: 0 var(--space-4) var(--space-4);
	}

	nav {
		display: flex;
		flex-direction: column;
		gap: var(--space-4);
	}

	nav h2 {
		margin-bottom: var(--space-1);
		color: var(--text-muted);
		font-size: var(--text-xs);
		font-weight: 650;
		letter-spacing: 0.05em;
		text-transform: uppercase;
	}

	nav ul {
		display: flex;
		flex-direction: column;
		gap: 1px;
		padding: 0;
		list-style: none;
	}

	nav a {
		display: block;
		margin: 0 calc(-1 * var(--space-2));
		padding: var(--space-1) var(--space-2);
		border-radius: var(--radius);
		color: var(--text);
		font-size: var(--text-md);
	}

	nav a:hover {
		background: var(--surface-2);
		text-decoration: none;
	}

	nav a[aria-current='page'] {
		background: var(--accent-soft);
		color: var(--accent);
		font-weight: 650;
	}

	.hint {
		color: var(--text-muted);
		font-size: var(--text-sm);
	}

	.account {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		margin-top: auto;
		padding-top: var(--space-4);
		border-top: 1px solid var(--border);
	}

	.who {
		overflow-wrap: anywhere;
		font-size: var(--text-sm);
		font-weight: 650;
	}

	.account-actions {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-2);
		font-size: var(--text-sm);
	}

	.live {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		color: var(--text-muted);
		font-size: var(--text-xs);
	}

	.live::before {
		content: '';
		flex: none;
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 50%;
		background: var(--border-strong);
	}

	.live.live::before {
		background: var(--success);
	}

	.live.offline::before {
		background: var(--warning);
	}

	main {
		width: 100%;
		max-width: var(--content-width);
		min-width: 0;
		padding: var(--space-6);
	}

	main:focus {
		outline: none;
	}

	@media (max-width: 52rem) {
		.shell {
			grid-template-columns: minmax(0, 1fr);
			/* The bar takes what it needs; the page gets the rest. */
			grid-template-rows: auto 1fr;
		}

		.sidebar {
			position: static;
			height: auto;
			border-right: 0;
			border-bottom: 1px solid var(--border);
		}

		.brand {
			padding: var(--space-3) var(--space-4);
		}

		.menu {
			display: inline-flex;
		}

		.body {
			display: none;
		}

		.body.open {
			display: flex;
		}

		main {
			padding: var(--space-4);
		}
	}
</style>
