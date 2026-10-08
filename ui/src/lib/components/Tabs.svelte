<script lang="ts">
	import { page } from '$app/state';
	import { carryQuery, currentTab, type Tab } from '#lib/nav.ts';

	/**
	 * Links between the sub-pages of one area. A tab with `match` stays current on the
	 * records below it; `carry` passes query parameters, such as the scope, on to every tab.
	 * `line` is an area's own bar above the title, `pills` a page's views below it.
	 */
	interface Props {
		label: string;
		tabs: Tab[];
		carry?: readonly string[];
		variant?: 'line' | 'pills';
	}

	let { label, tabs, carry = [], variant = 'line' }: Props = $props();

	const current = $derived(currentTab(tabs, page.url.pathname));
</script>

<nav class="tabs {variant}" aria-label={label}>
	{#each tabs as tab, index (tab.href)}
		<a
			href={carry.length > 0 ? carryQuery(tab.href, page.url.searchParams, carry) : tab.href}
			aria-current={index === current ? 'page' : undefined}
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
	}

	.line {
		border-bottom: 1px solid var(--border);
	}

	a {
		color: var(--text-muted);
		font-family: var(--font-display);
		font-size: var(--text-sm);
		font-weight: 600;
		letter-spacing: -0.01em;
		white-space: nowrap;
		transition:
			background-color 0.15s,
			border-color 0.15s,
			color 0.15s;
	}

	a:hover {
		color: var(--text);
		text-decoration: none;
	}

	.line a {
		margin-bottom: -1px;
		padding: var(--space-2) var(--space-3);
		border-bottom: 2px solid transparent;
	}

	.line a[aria-current='page'] {
		border-bottom-color: var(--accent);
		color: var(--accent);
	}

	.pills {
		gap: var(--space-2);
	}

	.pills a {
		padding: var(--space-1) var(--space-3);
		border: 1px solid var(--border);
		border-radius: 999px;
		font-size: var(--text-xs);
	}

	.pills a:hover {
		background: var(--surface-2);
	}

	.pills a[aria-current='page'] {
		border-color: var(--accent);
		background: var(--accent-soft);
		color: var(--accent);
	}
</style>
