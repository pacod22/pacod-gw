<script lang="ts">
	import House from '@lucide/svelte/icons/house';
	import type { Scope } from '#lib/access.ts';
	import { scopeTrail, type ScopeChoice } from '#lib/nav.ts';

	/**
	 * Where the page lies, as a trail in the top bar: the installation, an organisation, a
	 * workspace. A name leads to the home page of its level; the switcher at the head of the
	 * sidebar chooses another.
	 */
	interface Props {
		scope: Scope;
		/** The name of the top: "Platform", or "Home" for those without a platform role. */
		root: string;
		orgs: ScopeChoice[];
		workspaces: ScopeChoice[];
	}

	let { scope, root, orgs, workspaces }: Props = $props();

	const trail = $derived(scopeTrail(root, scope, orgs, workspaces));
	const last = $derived(trail.length - 1);
</script>

<nav class="scopebar" aria-label="Scope">
	<ol>
		{#each trail as step, index (step.level)}
			<li class:current={index === last}>
				<a
					href={step.href}
					class="name"
					class:root={index === 0}
					aria-current={index === last ? 'location' : undefined}
				>
					{#if index === 0}
						<House size={15} aria-hidden="true" />
						<span class="root-label">{step.label}</span>
					{:else}
						{step.label}
					{/if}
				</a>
			</li>
		{/each}
	</ol>
</nav>

<style>
	.scopebar {
		flex: 0 1 auto;
		min-width: 0;
	}

	ol {
		display: flex;
		align-items: center;
		min-width: 0;
		margin: 0;
		padding: 0;
		font-size: var(--text-sm);
		list-style: none;
	}

	li {
		display: flex;
		flex: 0 1 auto;
		align-items: center;
		gap: 2px;
		min-width: 0;
	}

	/* The top and the level shown keep their room; the names between them give way first. */
	li:first-child,
	li.current {
		flex-shrink: 0;
	}

	li + li::before {
		content: '/';
		flex: none;
		margin: 0 var(--space-1);
		color: var(--text-faint);
	}

	.name {
		display: block;
		min-width: 0;
		max-width: 16rem;
		overflow: hidden;
		padding: 0.25rem 0.375rem;
		border-radius: var(--radius-sm);
		color: var(--text-muted);
		font-weight: 500;
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.name:hover {
		background: var(--surface-2);
		color: var(--text);
		text-decoration: none;
	}

	.root {
		display: inline-flex;
		flex: none;
		align-items: center;
		gap: var(--space-2);
	}

	.current .name {
		color: var(--text);
		font-weight: 600;
	}

	@media (max-width: 64rem) {
		.name {
			max-width: 8rem;
		}
	}

	@media (max-width: 40rem) {
		.root-label {
			position: absolute;
			width: 1px;
			height: 1px;
			overflow: hidden;
			clip-path: inset(50%);
			white-space: nowrap;
		}
	}
</style>
