<script lang="ts">
	import { page } from '$app/state';

	/** Links between the sub-pages of one area. */
	interface Props {
		label: string;
		tabs: { label: string; href: string }[];
	}

	let { label, tabs }: Props = $props();
</script>

<nav class="tabs" aria-label={label}>
	{#each tabs as tab (tab.href)}
		<a
			href={tab.href}
			aria-current={page.url.pathname === tab.href.split('?')[0] ? 'page' : undefined}
		>
			{tab.label}
		</a>
	{/each}
</nav>

<style>
	.tabs {
		display: flex;
		gap: var(--space-1);
		overflow-x: auto;
		border-bottom: 1px solid var(--border);
	}

	a {
		margin-bottom: -1px;
		padding: var(--space-2) var(--space-3);
		border-bottom: 2px solid transparent;
		color: var(--text-muted);
		font-family: var(--font-display);
		font-size: var(--text-sm);
		font-weight: 600;
		letter-spacing: -0.01em;
		white-space: nowrap;
		transition:
			border-color 0.15s,
			color 0.15s;
	}

	a:hover {
		color: var(--text);
		text-decoration: none;
	}

	a[aria-current='page'] {
		border-bottom-color: var(--accent);
		color: var(--accent);
	}
</style>
