<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import House from '@lucide/svelte/icons/house';
	import type { Scope } from '#lib/access.ts';
	import { scopeTrail, switchTarget, type ScopeChoice } from '#lib/nav.ts';
	import ScopeCombobox from './ScopeCombobox.svelte';

	/**
	 * Where the page lies, as a trail in the top bar: the installation, an organisation, a
	 * workspace. A name leads to the home page of its level; the arrow beside it switches to
	 * another organisation or workspace and keeps the kind of page. Without `pickers` (narrow
	 * screens, where the drawer holds them) only the names are shown.
	 */
	interface Props {
		scope: Scope;
		/** The name of the top: "Platform", or "Home" for those without a platform role. */
		root: string;
		orgs: ScopeChoice[];
		workspaces: ScopeChoice[];
		pickers: boolean;
	}

	let { scope, root, orgs, workspaces, pickers }: Props = $props();

	const trail = $derived(scopeTrail(root, scope, orgs, workspaces));
	const last = $derived(trail.length - 1);

	function go(org: string, ws: string) {
		void goto(switchTarget({ org, ws, was: scope.org ?? '', from: page.url.pathname }));
	}
</script>

<nav class="scopebar" aria-label="Scope">
	<ol>
		<li class:current={last === 0}>
			<a href="/" class="name root" aria-current={last === 0 ? 'location' : undefined}>
				<House size={15} aria-hidden="true" />
				<span class="root-label">{root}</span>
			</a>
		</li>
		{#if scope.org || (pickers && orgs.length > 0)}
			<li class:current={last === 1}>
				{#if trail[1]}
					<a class="name" href={trail[1].href} aria-current={last === 1 ? 'location' : undefined}>
						{trail[1].label}
					</a>
				{/if}
				{#if pickers}
					<ScopeCombobox
						id="switch-org"
						variant="crumb"
						label="Organisation"
						choices={orgs}
						value={scope.org ?? ''}
						placeholder="Choose an organisation"
						noneLabel="No organisation"
						onpick={(org) => go(org, '')}
					/>
				{/if}
			</li>
		{/if}
		{#if scope.org && (scope.ws || (pickers && workspaces.length > 0))}
			<li class:current={last === 2}>
				{#if trail[2]}
					<a class="name" href={trail[2].href} aria-current={last === 2 ? 'location' : undefined}>
						{trail[2].label}
					</a>
				{/if}
				{#if pickers}
					<ScopeCombobox
						id="switch-ws"
						variant="crumb"
						label="Workspace"
						choices={workspaces}
						value={scope.ws ?? ''}
						placeholder="Choose a workspace"
						noneLabel="No workspace: the organisation"
						onpick={(ws) => go(scope.org ?? '', ws)}
					/>
				{/if}
			</li>
		{/if}
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

	/* Narrow screens: the pickers are in the drawer; this also holds before hydration. */
	@media (max-width: 64rem) {
		.scopebar :global(.combobox) {
			display: none;
		}

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
