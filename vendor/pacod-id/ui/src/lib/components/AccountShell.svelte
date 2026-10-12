<script lang="ts">
	import type { Snippet } from 'svelte';
	import { BrandMark, BRANDS, Button, initials } from '@pacod/ui';
	import { page } from '$app/state';
	import { resolve } from '$app/paths';
	import { m } from '#lib/paraglide/messages.js';
	import { brand } from '#lib/page.ts';
	import { me, signOut } from '#lib/me.svelte.ts';
	import PageFooter from '#lib/components/PageFooter.svelte';

	let { children }: { children: Snippet } = $props();
	const b = brand();

	const accountLinks = $derived([
		{ path: '/account' as const, label: m.nav_profile() },
		{ path: '/account/security' as const, label: m.nav_security() },
		{ path: '/account/sessions' as const, label: m.nav_sessions() },
		{ path: '/account/connections' as const, label: m.nav_connections() },
		{ path: '/account/organisations' as const, label: m.nav_organisations() }
	]);
	const adminLinks = $derived(
		[
			me.value?.admin.accounts && { path: '/admin' as const, label: m.nav_admin_accounts() },
			me.value?.admin.service_accounts && {
				path: '/admin/service-accounts' as const,
				label: m.nav_admin_service_accounts()
			},
			me.value?.admin.audit && { path: '/admin/audit' as const, label: m.nav_admin_audit() }
		].filter((l) => !!l) as {
			path: '/admin' | '/admin/service-accounts' | '/admin/audit';
			label: string;
		}[]
	);
	const name = $derived(me.value?.account.display_name || me.value?.account.email || '');
</script>

<div class="shell">
	<header>
		<a class="brand" href={resolve('/account')}>
			<BrandMark brand={b} size={32} />
			<span>{BRANDS[b].wordmark}</span>
		</a>
		{#if me.value}
			<div class="who">
				<span class="avatar" aria-hidden="true">{initials(name)}</span>
				<span class="name">{name}</span>
				<Button variant="quiet" onclick={signOut}>{m.nav_sign_out()}</Button>
			</div>
		{/if}
	</header>
	<div class="body">
		<nav aria-label={m.nav_account()}>
			<p class="section">{m.nav_account()}</p>
			{#each accountLinks as l (l.path)}
				<a
					href={resolve(l.path)}
					aria-current={page.url.pathname === resolve(l.path) ? 'page' : undefined}>{l.label}</a
				>
			{/each}
			{#if adminLinks.length}
				<p class="section">{m.nav_admin()}</p>
				{#each adminLinks as l (l.path)}
					<a
						href={resolve(l.path)}
						aria-current={page.url.pathname === resolve(l.path) ? 'page' : undefined}>{l.label}</a
					>
				{/each}
			{/if}
		</nav>
		<main>
			{@render children()}
			<PageFooter />
		</main>
	</div>
</div>

<style>
	.shell {
		min-height: 100vh;
		display: flex;
		flex-direction: column;
	}

	header {
		display: flex;
		justify-content: space-between;
		align-items: center;
		padding: var(--pid-space-3) var(--pid-space-5);
		border-bottom: 1px solid var(--pid-border);
		background: var(--pid-surface);
	}

	.brand {
		display: flex;
		align-items: center;
		gap: var(--pid-space-3);
		color: var(--pid-text);
		text-decoration: none;
		font-family: var(--pid-font-display);
		font-weight: 700;
	}

	.who {
		display: flex;
		align-items: center;
		gap: var(--pid-space-3);
	}

	.avatar {
		display: inline-grid;
		place-items: center;
		width: 2rem;
		height: 2rem;
		border-radius: 50%;
		background: var(--pid-accent-soft);
		color: var(--pid-accent);
		font-size: var(--pid-text-sm);
		font-weight: 700;
	}

	.body {
		flex: 1;
		display: grid;
		grid-template-columns: 14rem 1fr;
		gap: var(--pid-space-6);
		padding: var(--pid-space-5);
		max-width: 72rem;
		width: 100%;
		box-sizing: border-box;
		margin: 0 auto;
	}

	nav {
		display: flex;
		flex-direction: column;
		gap: var(--pid-space-1);
	}

	nav .section {
		margin: var(--pid-space-4) 0 var(--pid-space-1);
		font-size: var(--pid-text-xs);
		text-transform: uppercase;
		letter-spacing: 0.06em;
		color: var(--pid-text-faint);
	}

	nav a {
		padding: var(--pid-space-2) var(--pid-space-3);
		border-radius: var(--pid-radius-sm);
		color: var(--pid-text-muted);
		text-decoration: none;
	}

	nav a:hover {
		background: var(--pid-surface-2);
		color: var(--pid-text);
	}

	nav a[aria-current='page'] {
		background: var(--pid-accent-soft);
		color: var(--pid-accent);
		font-weight: 600;
	}

	main {
		min-width: 0;
	}

	@media (max-width: 760px) {
		.body {
			grid-template-columns: 1fr;
		}

		nav {
			flex-direction: row;
			flex-wrap: wrap;
		}

		nav .section {
			width: 100%;
		}

		.name {
			display: none;
		}
	}
</style>
