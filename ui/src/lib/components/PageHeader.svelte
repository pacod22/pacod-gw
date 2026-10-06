<script lang="ts">
	import type { Snippet } from 'svelte';
	import { page } from '$app/state';
	import { scopeCrumbs } from '#lib/nav.ts';

	interface Crumb {
		label: string;
		href: string;
	}

	interface Props {
		title: string;
		description?: string;
		/**
		 * The pages above this one, below its organisation and workspace: those are put in
		 * front by themselves.
		 */
		crumbs?: Crumb[];
		actions?: Snippet;
	}

	let { title, description, crumbs = [], actions }: Props = $props();

	const all = $derived([
		...scopeCrumbs(
			page.url.pathname,
			(page.data.orgs as { id: string; label: string }[] | undefined) ?? [],
			(page.data.workspaces as { id: string; label: string }[] | undefined) ?? []
		),
		...crumbs
	]);
</script>

<svelte:head>
	<title>{title} · pacod-gw</title>
</svelte:head>

<header class="page-header">
	<div class="titles">
		{#if all.length > 0}
			<nav aria-label="Breadcrumb" class="crumbs">
				{#each all as crumb (crumb.href)}
					<a href={crumb.href}>{crumb.label}</a><span aria-hidden="true">/</span>
				{/each}
			</nav>
		{/if}
		<h1>{title}</h1>
		{#if description}<p class="muted">{description}</p>{/if}
	</div>
	{#if actions}<div class="row">{@render actions()}</div>{/if}
</header>

<style>
	.page-header {
		display: flex;
		flex-wrap: wrap;
		align-items: flex-end;
		justify-content: space-between;
		gap: var(--space-3) var(--space-4);
	}

	.titles {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		min-width: 0;
		max-width: 48rem;
	}

	.crumbs {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
		margin-bottom: var(--space-1);
		color: var(--text-faint);
		font-family: var(--font-mono);
		font-size: 0.6875rem;
		font-weight: 500;
		letter-spacing: 0.12em;
		text-transform: uppercase;
	}

	.crumbs a {
		color: var(--text-muted);
	}

	.crumbs a:hover {
		color: var(--accent);
	}

	.titles p {
		font-size: var(--text-sm);
	}
</style>
