<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import type { Scope } from '#lib/access.ts';
	import { switchTarget, type ScopeChoice } from '#lib/nav.ts';
	import ScopeCombobox from './ScopeCombobox.svelte';

	/**
	 * Chooses the organisation and the workspace the console works in. The choice is part of
	 * the URL, so choosing navigates; nothing is remembered in the browser. Without
	 * JavaScript, two plain lists and a button do the same through `/switch`.
	 */
	interface Props {
		scope: Scope;
		orgs: ScopeChoice[];
		workspaces: ScopeChoice[];
	}

	let { scope, orgs, workspaces }: Props = $props();

	function go(org: string, ws: string) {
		void goto(switchTarget({ org, ws, was: scope.org ?? '', from: page.url.pathname }));
	}
</script>

<div class="switcher">
	<ScopeCombobox
		id="switch-org"
		label="Organisation"
		choices={orgs}
		value={scope.org ?? ''}
		placeholder={orgs.length > 0 ? 'Choose an organisation' : 'No organisations yet'}
		disabled={orgs.length === 0}
		onpick={(org) => go(org, '')}
	/>
	<ScopeCombobox
		id="switch-ws"
		label="Workspace"
		choices={workspaces}
		value={scope.ws ?? ''}
		placeholder={!scope.org
			? 'Choose an organisation first'
			: workspaces.length > 0
				? 'Choose a workspace'
				: 'No workspaces yet'}
		noneLabel="No workspace: the organisation"
		disabled={!scope.org || workspaces.length === 0}
		onpick={(ws) => go(scope.org ?? '', ws)}
	/>
</div>

<noscript>
	<form method="GET" action="/switch" class="switcher fallback">
		<input type="hidden" name="from" value={page.url.pathname} />
		<input type="hidden" name="was" value={scope.org ?? ''} />
		<label for="switch-org-plain">Organisation</label>
		<select id="switch-org-plain" name="org" value={scope.org ?? ''}>
			<option value="">None selected</option>
			{#each orgs as org (org.id)}<option value={org.id}>{org.label}</option>{/each}
		</select>
		<label for="switch-ws-plain">Workspace</label>
		<select id="switch-ws-plain" name="ws" value={scope.ws ?? ''}>
			<option value="">None selected</option>
			{#each workspaces as ws (ws.id)}<option value={ws.id}>{ws.label}</option>{/each}
		</select>
		<button type="submit" class="btn">Switch</button>
	</form>
</noscript>

<style>
	.switcher {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
	}

	.fallback {
		margin-top: var(--space-3);
	}
</style>
