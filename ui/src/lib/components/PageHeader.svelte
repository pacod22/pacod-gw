<script lang="ts">
	import type { Snippet } from 'svelte';

	interface Crumb {
		label: string;
		href: string;
	}

	interface Props {
		title: string;
		description?: string;
		/** The pages above this one. */
		crumbs?: Crumb[];
		actions?: Snippet;
	}

	let { title, description, crumbs = [], actions }: Props = $props();
</script>

<svelte:head>
	<title>{title} · pacod-gw</title>
</svelte:head>

<header class="page-header">
	<div class="titles">
		{#if crumbs.length > 0}
			<nav aria-label="Breadcrumb" class="crumbs">
				{#each crumbs as crumb (crumb.href)}
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
		color: var(--text-muted);
		font-size: var(--text-sm);
	}
</style>
