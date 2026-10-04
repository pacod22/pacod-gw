<script lang="ts">
	import { page } from '$app/state';

	const links = [
		{ href: '/', label: 'Dashboard' },
		{ href: '/providers', label: 'Providers' },
		{ href: '/routes', label: 'Routes' },
		{ href: '/keys', label: 'API Keys' },
		{ href: '/analytics', label: 'Analytics' },
		{ href: '/reports', label: 'Reports' },
		{ href: '/settings', label: 'Settings' }
	];
</script>

<nav>
	<strong>pacod-gw</strong>
	{#each links as link (link.href)}
		<a href={link.href} aria-current={page.url.pathname === link.href ? 'page' : undefined}>
			{link.label}
		</a>
	{/each}
</nav>

<style>
	nav {
		display: flex;
		flex-direction: column;
		gap: 0.5rem;
		padding: 1rem;
		min-width: 12rem;
		border-right: 1px solid #ddd;
	}
	a[aria-current='page'] {
		font-weight: 600;
	}
</style>
