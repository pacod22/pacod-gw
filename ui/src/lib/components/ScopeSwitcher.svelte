<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import type { Scope } from '#lib/access.ts';
	import { switchTarget, type ScopeChoice } from '#lib/nav.ts';
	import ScopeCombobox from './ScopeCombobox.svelte';

	/**
	 * Chooses the organisation and the workspace the console works in, at the head of the
	 * sidebar's part for them. The choice is part of the URL, so choosing navigates; the layout
	 * keeps the last one for the pages that have no scope of their own, such as the platform's.
	 */
	interface Props {
		scope: Scope;
		orgs: ScopeChoice[];
		workspaces: ScopeChoice[];
		/** Called before leading to the whole installation, so that the kept scope goes too. */
		onforget: () => void;
	}

	let { scope, orgs, workspaces, onforget }: Props = $props();

	function go(org: string, ws: string) {
		if (!org) onforget();
		void goto(switchTarget({ org, ws, was: scope.org ?? '', from: page.url.pathname }));
	}
</script>

<div class="switcher">
	<!-- Nothing chosen stands for all of them, which is what the pages below then show. -->
	<ScopeCombobox
		id="switch-org"
		label="Organisation"
		choices={orgs}
		value={scope.org ?? ''}
		placeholder={orgs.length > 0 ? 'All organisations' : 'No organisations yet'}
		noneLabel="All organisations"
		disabled={orgs.length === 0}
		onpick={(org) => go(org, '')}
	/>
	<ScopeCombobox
		id="switch-ws"
		label="Workspace"
		choices={workspaces}
		value={scope.ws ?? ''}
		placeholder={scope.org && workspaces.length === 0 ? 'No workspaces yet' : 'All workspaces'}
		noneLabel="All workspaces"
		disabled={!scope.org || workspaces.length === 0}
		onpick={(ws) => go(scope.org ?? '', ws)}
	/>
</div>

<style>
	.switcher {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
	}
</style>
