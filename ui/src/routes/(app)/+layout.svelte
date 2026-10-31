<script lang="ts">
	import { onMount, tick, untrack } from 'svelte';
	import { afterNavigate } from '$app/navigation';
	import { page } from '$app/state';
	import ChevronDown from '@lucide/svelte/icons/chevron-down';
	import LockKeyhole from '@lucide/svelte/icons/lock-keyhole';
	import LogOut from '@lucide/svelte/icons/log-out';
	import Menu from '@lucide/svelte/icons/menu';
	import Moon from '@lucide/svelte/icons/moon';
	import Sun from '@lucide/svelte/icons/sun';
	import UserRound from '@lucide/svelte/icons/user-round';
	import X from '@lucide/svelte/icons/x';
	import { describeRole, hasPlatformRole, type Scope } from '#lib/access.ts';
	import ScopeBar from '#lib/components/ScopeBar.svelte';
	import ScopeFallback from '#lib/components/ScopeFallback.svelte';
	import ScopeSwitcher from '#lib/components/ScopeSwitcher.svelte';
	import { startLiveUpdates, type LiveState } from '#lib/live.ts';
	import { navIcon } from '#lib/nav-icons.ts';
	import {
		buildNav,
		isCurrent,
		keptAfter,
		keptScopeCookie,
		ownsScope,
		pageScope,
		platformNavCookie,
		type NavGroup,
		type ScopeChoice
	} from '#lib/nav.ts';
	import type { LayoutProps } from './$types';

	let { data, children }: LayoutProps = $props();

	/**
	 * The scope the sidebar keeps for the pages without one of their own, with its workspaces.
	 * The cookie seeds it on a full load; from then on the pages shown keep it current, since
	 * the load of this layout does not run again for every page.
	 */
	let kept: { scope: Scope; workspaces: ScopeChoice[] } = $state(
		untrack(() => ({ scope: data.kept, workspaces: data.keptWorkspaces }))
	);
	let platformOpen = $state(untrack(() => data.platformOpen));

	// The page's own scope comes from the URL alone. The sidebar shows it where the page has
	// one, and the kept scope elsewhere, so that the platform's pages leave it as it is.
	const owns = $derived(ownsScope(page.url.pathname, page.url.searchParams));
	const shown = $derived(pageScope(page.url.pathname, data.scope));
	const scope = $derived(owns ? shown : kept.scope);
	const workspaces = $derived(owns ? data.workspaces : kept.workspaces);
	const nav = $derived(buildNav(data.principal, scope));
	/** The platform page shown, named on the folded block so that the place is not lost. */
	const platformPage = $derived(
		nav.platform.flatMap((group) => group.items).find((item) => isCurrent(item, page.url.pathname))
	);
	const roles = $derived(data.principal.roles.map(describeRole));
	const initial = $derived(data.principal.display.trim().charAt(0).toUpperCase() || '?');

	let menuOpen = $state(false);
	let accountOpen = $state(false);
	let live: LiveState = $state('connecting');
	let theme: 'light' | 'dark' = $state('light');

	// Runs in the browser only, for the page shown: a page loaded ahead because a link was
	// hovered must not change what is kept. It follows the workspaces too, so that one created
	// or renamed here is offered on the platform's pages.
	$effect(() => {
		const next = keptAfter(page.url.pathname, owns, shown);
		if (next) keep(next, next.org ? data.workspaces : []);
	});

	function keep(next: Scope, choices: ScopeChoice[]) {
		kept = { scope: next, workspaces: choices };
		document.cookie = keptScopeCookie(next, location.protocol === 'https:');
	}

	function togglePlatform(event: Event) {
		platformOpen = (event.currentTarget as HTMLDetailsElement).open;
		document.cookie = platformNavCookie(platformOpen, location.protocol === 'https:');
	}

	let menuButton: HTMLButtonElement | undefined = $state();
	let closeButton: HTMLButtonElement | undefined = $state();
	let accountEl: HTMLDivElement | undefined = $state();

	afterNavigate(() => {
		menuOpen = false;
		accountOpen = false;
	});

	onMount(() => {
		const chosen = document.documentElement.dataset.theme;
		theme =
			chosen === 'dark' || chosen === 'light'
				? chosen
				: matchMedia('(prefers-color-scheme: dark)').matches
					? 'dark'
					: 'light';
		return startLiveUpdates({
			onstate: (state) => {
				live = state;
			}
		});
	});

	function toggleTheme() {
		theme = theme === 'dark' ? 'light' : 'dark';
		document.documentElement.dataset.theme = theme;
		try {
			localStorage.setItem('pgw-theme', theme);
		} catch {
			// Storage can be blocked; the choice then lasts until the page is reloaded.
		}
	}

	async function openMenu() {
		menuOpen = true;
		await tick();
		closeButton?.focus();
	}

	function closeMenu() {
		if (!menuOpen) return;
		menuOpen = false;
		menuButton?.focus();
	}

	function onWindowKeydown(event: KeyboardEvent) {
		if (event.key !== 'Escape') return;
		if (accountOpen) {
			accountOpen = false;
			accountEl?.querySelector<HTMLButtonElement>('.account-button')?.focus();
		} else {
			closeMenu();
		}
	}

	function onWindowClick(event: MouseEvent) {
		if (accountOpen && accountEl && !accountEl.contains(event.target as Node)) {
			accountOpen = false;
		}
	}

	const LIVE_TEXT: Record<LiveState, string> = {
		connecting: 'Connecting to live updates…',
		live: 'Live updates are on',
		offline: 'Live updates are off. Reload to reconnect.'
	};
</script>

<svelte:window onkeydown={onWindowKeydown} onclick={onWindowClick} />

<a class="skip" href="#main">Skip to the content</a>

<div class="shell">
	{#if menuOpen}
		<div class="scrim" role="presentation" onclick={closeMenu}></div>
	{/if}

	<aside id="sidebar" class="sidebar" class:open={menuOpen} aria-label="Navigation">
		<div class="brand">
			<a href="/" class="brand-link" aria-label="pacod-gw, AI Gateway: dashboard">
				<span class="mark" aria-hidden="true">GW<span class="dot"></span></span>
				<span class="brand-text">
					<span class="brand-name">pacod-gw</span>
					<span class="brand-sub">AI Gateway</span>
				</span>
			</a>
			<button
				type="button"
				class="icon-button close"
				aria-label="Close the navigation"
				bind:this={closeButton}
				onclick={closeMenu}
			>
				<X size={16} />
			</button>
		</div>

		{#snippet navGroups(groups: NavGroup[])}
			{#each groups as group (group.id)}
				<section>
					{#if group.label}<h2>{group.label}</h2>{/if}
					<ul>
						{#each group.items as item (item.href)}
							{@const Icon = navIcon(item.icon)}
							<li>
								<a
									href={item.href}
									aria-current={isCurrent(item, page.url.pathname) ? 'page' : undefined}
								>
									<Icon size={16} aria-hidden="true" />
									<span>{item.label}</span>
								</a>
							</li>
						{/each}
					</ul>
				</section>
			{/each}
		{/snippet}

		{#if nav.platform.length > 0}
			<nav class="platform" aria-label="Platform">
				<details open={platformOpen} ontoggle={togglePlatform}>
					<summary>
						<span class="platform-label">Platform</span>
						{#if !platformOpen && platformPage}
							<span class="platform-page">{platformPage.label}</span>
						{/if}
						<span class="fold" aria-hidden="true"><ChevronDown size={14} /></span>
					</summary>
					<div class="groups">{@render navGroups(nav.platform)}</div>
				</details>
			</nav>
		{/if}

		<ScopeFallback {scope} orgs={data.orgs} {workspaces} />
		<div class="scope">
			<ScopeSwitcher
				{scope}
				orgs={data.orgs}
				{workspaces}
				onforget={() => keep({ org: null, ws: null }, [])}
			/>
		</div>

		<nav class="main" aria-label="Main">{@render navGroups(nav.groups)}</nav>
	</aside>

	<div class="column">
		<header class="topbar">
			<button
				type="button"
				class="icon-button hamburger"
				aria-label="Open the navigation"
				aria-expanded={menuOpen}
				aria-controls="sidebar"
				bind:this={menuButton}
				onclick={openMenu}
			>
				<Menu size={20} />
			</button>

			<!-- Where the page itself lies: a platform page reads "Platform" whatever is kept. -->
			<ScopeBar
				scope={shown}
				root={hasPlatformRole(data.principal) ? 'Platform' : 'Home'}
				orgs={data.orgs}
				workspaces={data.workspaces}
			/>

			<div class="spacer"></div>

			<p class="live {live}" role="status" title={LIVE_TEXT[live]}>
				<span class="live-text">{LIVE_TEXT[live]}</span>
			</p>

			<button
				type="button"
				class="icon-button"
				aria-label={theme === 'dark' ? 'Switch to the light theme' : 'Switch to the dark theme'}
				onclick={toggleTheme}
			>
				{#if theme === 'dark'}<Sun size={18} />{:else}<Moon size={18} />{/if}
			</button>

			<div class="account" bind:this={accountEl}>
				<button
					type="button"
					class="account-button"
					aria-expanded={accountOpen}
					aria-controls="account-panel"
					onclick={() => (accountOpen = !accountOpen)}
				>
					<span class="who">
						<span class="who-name">{data.principal.display}</span>
						<span class="who-role">
							{roles[0] ?? 'No role yet'}{#if roles.length > 1}, +{roles.length - 1}{/if}
						</span>
					</span>
					<span class="avatar" aria-hidden="true">{initial}</span>
					<span class="chevron" class:turned={accountOpen}><ChevronDown size={14} /></span>
					<span class="visually-hidden">Account</span>
				</button>

				{#if accountOpen}
					<div id="account-panel" class="account-panel">
						<div class="panel-head">
							<p class="panel-name" title={data.principal.id}>{data.principal.display}</p>
							<p class="panel-roles">
								{roles.length > 0 ? roles.join(', ') : 'No role yet'}
							</p>
						</div>
						<div class="panel-actions">
							{#if data.identity}
								<a href={data.identity.account} class="panel-item">
									<UserRound size={15} aria-hidden="true" />
									Your account
								</a>
							{:else}
								<a href="/account/password" class="panel-item">
									<LockKeyhole size={15} aria-hidden="true" />
									Change password
								</a>
							{/if}
							<form method="POST" action="/logout">
								<button type="submit" class="panel-item danger">
									<LogOut size={15} aria-hidden="true" />
									Sign out
								</button>
							</form>
						</div>
					</div>
				{/if}
			</div>
		</header>

		<main id="main" tabindex="-1">
			{@render children()}
		</main>
	</div>
</div>

<style>
	.skip {
		position: absolute;
		left: var(--space-2);
		top: -4rem;
		z-index: 50;
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

	/* Sidebar */

	.sidebar {
		position: sticky;
		top: 0;
		z-index: 30;
		align-self: start;
		display: flex;
		flex-direction: column;
		height: 100dvh;
		overflow-y: auto;
		border-right: 1px solid var(--sidebar-border);
		background: var(--sidebar);
		color: var(--sidebar-text);
	}

	.brand {
		display: flex;
		flex: none;
		align-items: center;
		gap: var(--space-3);
		height: var(--topbar-height);
		padding: 0 var(--space-4);
		border-bottom: 1px solid var(--sidebar-border);
	}

	.brand-link {
		display: flex;
		flex: 1;
		align-items: center;
		gap: var(--space-3);
		min-width: 0;
		color: inherit;
	}

	.brand-link:hover {
		color: inherit;
		text-decoration: none;
	}

	.mark {
		position: relative;
		display: grid;
		flex: none;
		place-items: center;
		width: 2rem;
		height: 2rem;
		border-radius: var(--radius);
		background: var(--accent);
		color: #ffffff;
		font-family: var(--font-display);
		font-size: 0.75rem;
		font-weight: 800;
		letter-spacing: -0.02em;
	}

	.dot {
		position: absolute;
		right: -2px;
		bottom: -2px;
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 50%;
		background: var(--gold);
		box-shadow: 0 0 0 2px var(--sidebar);
	}

	.brand-text {
		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	.brand-name {
		overflow: hidden;
		font-family: var(--font-display);
		font-size: 0.875rem;
		font-weight: 700;
		line-height: 1.25;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.brand-sub {
		color: var(--sidebar-muted);
		font-family: var(--font-mono);
		font-size: 10px;
		letter-spacing: 0.14em;
		line-height: 1.25;
		text-transform: uppercase;
	}

	.scope {
		padding: var(--space-4) var(--space-4) var(--space-2);
	}

	/* Without JavaScript the pickers cannot open; the plain form of ScopeFallback stands in. */
	@media (scripting: none) {
		.scope {
			display: none;
		}
	}

	.main,
	.groups {
		display: flex;
		flex-direction: column;
		gap: var(--space-4);
	}

	.main {
		padding: var(--space-3) var(--space-2) var(--space-5);
	}

	/* The Platform block: the installation's pages, the same on every level. It folds. */
	.platform {
		flex: none;
		padding: var(--space-2) var(--space-2) var(--space-3);
		border-bottom: 1px solid var(--sidebar-border);
	}

	.platform summary {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		padding: var(--space-2) var(--space-3);
		border-radius: 0.5rem;
		color: var(--sidebar-text);
		font-family: var(--font-display);
		font-size: 0.75rem;
		font-weight: 700;
		letter-spacing: 0.04em;
		list-style: none;
		cursor: pointer;
	}

	.platform summary::-webkit-details-marker {
		display: none;
	}

	.platform summary:hover {
		background: var(--sidebar-hover);
	}

	.platform-label {
		flex: none;
	}

	/* The page shown, when it lies in the folded block. */
	.platform-page {
		min-width: 0;
		overflow: hidden;
		color: var(--sidebar-active-text);
		font-weight: 500;
		letter-spacing: 0;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.fold {
		display: inline-flex;
		flex: none;
		margin-left: auto;
		color: var(--sidebar-muted);
		transition: transform 0.15s;
	}

	details:not([open]) .fold {
		transform: rotate(-90deg);
	}

	.groups {
		padding-top: var(--space-2);
	}

	nav h2 {
		padding: 2px var(--space-3) var(--space-1);
		color: var(--sidebar-muted);
		font-family: var(--font-mono);
		font-size: 10px;
		font-weight: 600;
		letter-spacing: 0.12em;
		line-height: 1.5;
		text-transform: uppercase;
	}

	nav ul {
		display: flex;
		flex-direction: column;
		gap: 2px;
		margin: 0;
		padding: 0;
		list-style: none;
	}

	nav a {
		position: relative;
		display: flex;
		align-items: center;
		gap: var(--space-3);
		padding: var(--space-2) var(--space-3);
		border-radius: 0.5rem;
		color: var(--sidebar-muted);
		font-size: 0.875rem;
		font-weight: 500;
		transition:
			background-color 0.15s,
			color 0.15s;
	}

	nav a :global(svg) {
		flex: none;
	}

	nav a span {
		min-width: 0;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	nav a:hover {
		background: var(--sidebar-hover);
		color: var(--sidebar-text);
		text-decoration: none;
	}

	nav a[aria-current='page'] {
		background: var(--sidebar-active);
		color: var(--sidebar-active-text);
	}

	nav a[aria-current='page']::before {
		content: '';
		position: absolute;
		top: 50%;
		left: 0;
		width: 2px;
		height: 1rem;
		border-radius: 0 999px 999px 0;
		background: var(--accent);
		transform: translateY(-50%);
	}

	/* Top bar */

	.column {
		display: flex;
		flex-direction: column;
		min-width: 0;
	}

	.topbar {
		position: sticky;
		top: 0;
		z-index: 20;
		display: flex;
		align-items: center;
		gap: var(--space-3);
		height: var(--topbar-height);
		padding: 0 var(--space-6);
		border-bottom: 1px solid var(--border);
		background: color-mix(in srgb, var(--bg) 88%, transparent);
		backdrop-filter: blur(8px);
	}

	.spacer {
		flex: 1;
	}

	.icon-button {
		display: inline-grid;
		flex: none;
		place-items: center;
		width: 2.25rem;
		height: 2.25rem;
		padding: 0;
		border: 0;
		border-radius: 0.5rem;
		background: transparent;
		color: var(--text-muted);
		cursor: pointer;
		transition:
			background-color 0.15s,
			color 0.15s;
	}

	.icon-button:hover {
		background: var(--surface-2);
		color: var(--text);
	}

	.hamburger,
	.close {
		display: none;
	}

	.live {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		min-width: 0;
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
		box-shadow: 0 0 0 3px var(--success-soft);
	}

	.live.offline::before {
		background: var(--gold);
		box-shadow: 0 0 0 3px var(--warning-soft);
	}

	.live-text {
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	/* Account */

	.account {
		position: relative;
	}

	.account-button {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		padding: var(--space-1) var(--space-2) var(--space-1) var(--space-3);
		border: 0;
		border-radius: 999px;
		background: transparent;
		color: var(--text);
		cursor: pointer;
		transition: background-color 0.15s;
	}

	.account-button:hover {
		background: var(--surface-2);
	}

	.account-button:focus-visible {
		border-radius: 999px;
	}

	.who {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		min-width: 0;
		max-width: 14rem;
		line-height: 1.2;
	}

	.who-name,
	.who-role {
		max-width: 100%;
		overflow: hidden;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.who-name {
		font-size: var(--text-sm);
		font-weight: 600;
	}

	.who-role {
		margin-top: 2px;
		color: var(--text-muted);
		font-size: var(--text-xs);
	}

	.avatar {
		display: grid;
		flex: none;
		place-items: center;
		width: 2rem;
		height: 2rem;
		border-radius: 50%;
		background: var(--accent-soft);
		color: var(--accent);
		font-family: var(--font-display);
		font-size: var(--text-sm);
		font-weight: 700;
	}

	.chevron {
		display: inline-flex;
		color: var(--text-muted);
		transition: transform 0.15s;
	}

	.chevron.turned {
		transform: rotate(180deg);
	}

	.account-panel {
		position: absolute;
		top: calc(100% + var(--space-2));
		right: 0;
		z-index: 40;
		width: 16rem;
		overflow: hidden;
		border: 1px solid var(--border);
		border-radius: var(--radius-lg);
		background: var(--surface);
		box-shadow: var(--shadow);
	}

	.panel-head {
		padding: var(--space-3);
		border-bottom: 1px solid var(--border);
	}

	.panel-name {
		overflow: hidden;
		font-size: var(--text-sm);
		font-weight: 600;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.panel-roles {
		color: var(--text-muted);
		font-size: var(--text-xs);
	}

	.panel-actions {
		padding: 0.375rem;
	}

	.panel-item {
		display: flex;
		align-items: center;
		gap: 0.625rem;
		width: 100%;
		padding: var(--space-2) 0.625rem;
		border: 0;
		border-radius: 0.5rem;
		background: transparent;
		color: var(--text);
		font-size: var(--text-sm);
		text-align: left;
		cursor: pointer;
	}

	.panel-item:hover {
		background: var(--surface-2);
		color: var(--text);
		text-decoration: none;
	}

	.panel-item :global(svg) {
		flex: none;
		color: var(--text-muted);
	}

	.panel-item.danger,
	.panel-item.danger :global(svg) {
		color: var(--danger);
	}

	.panel-item.danger:hover {
		background: var(--danger-soft);
	}

	/* Page */

	main {
		width: 100%;
		max-width: var(--content-width);
		min-width: 0;
		padding: var(--space-6);
	}

	main:focus {
		outline: none;
	}

	/* Narrow screens: the sidebar becomes a drawer behind the menu button. */
	@media (max-width: 64rem) {
		.shell {
			grid-template-columns: minmax(0, 1fr);
		}

		.sidebar {
			position: fixed;
			inset: 0 auto 0 0;
			width: min(var(--sidebar-width), 85vw);
			height: 100dvh;
			box-shadow: var(--shadow);
			transform: translateX(-100%);
			visibility: hidden;
			transition:
				transform 0.2s ease-in-out,
				visibility 0.2s;
		}

		/* Visible at once when opening, so that focus can move into it; hidden only after closing. */
		.sidebar.open {
			transform: none;
			visibility: visible;
			transition:
				transform 0.2s ease-in-out,
				visibility 0s;
		}

		.scrim {
			position: fixed;
			inset: 0;
			z-index: 25;
			background: var(--backdrop);
			backdrop-filter: blur(4px);
		}

		.hamburger,
		.close {
			display: inline-grid;
		}

		.topbar {
			padding: 0 var(--space-4);
		}

		/* The avatar is enough here; the scope bar needs the room. The panel names the account. */
		.who {
			display: none;
		}

		main {
			padding: var(--space-5) var(--space-4);
		}
	}

	/* The scope bar needs the room: the dot is enough to see; the words stay for screen readers. */
	@media (max-width: 80rem) {
		.live-text {
			position: absolute;
			width: 1px;
			height: 1px;
			overflow: hidden;
			clip-path: inset(50%);
		}

		.live {
			flex: none;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.sidebar,
		.fold {
			transition: none;
		}
	}
</style>
