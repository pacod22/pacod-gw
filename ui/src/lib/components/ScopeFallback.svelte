<script lang="ts">
	import { page } from '$app/state';
	import type { Scope } from '#lib/access.ts';
	import type { ScopeChoice } from '#lib/nav.ts';

	/**
	 * Choosing the organisation and the workspace without JavaScript: two plain lists and a
	 * button that go through `/switch`.
	 */
	interface Props {
		scope: Scope;
		orgs: ScopeChoice[];
		workspaces: ScopeChoice[];
	}

	let { scope, orgs, workspaces }: Props = $props();
</script>

<noscript>
	<form method="GET" action="/switch" class="fallback">
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
	.fallback {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		padding: var(--space-4) var(--space-4) 0;
	}
</style>
