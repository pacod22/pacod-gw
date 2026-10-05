<script lang="ts">
	import { goto } from '$app/navigation';
	import { page } from '$app/state';
	import type { Scope } from '#lib/access.ts';
	import { switchTarget } from '#lib/nav.ts';

	/**
	 * Chooses the organisation and the workspace the console works in. The choice is part of
	 * the URL, so choosing navigates; nothing is remembered in the browser.
	 */
	interface Choice {
		id: string;
		label: string;
	}

	interface Props {
		scope: Scope;
		orgs: Choice[];
		workspaces: Choice[];
	}

	let { scope, orgs, workspaces }: Props = $props();

	let orgSelect: HTMLSelectElement | undefined = $state();
	let wsSelect: HTMLSelectElement | undefined = $state();

	function go() {
		void goto(
			switchTarget({
				org: orgSelect?.value ?? '',
				ws: wsSelect?.value ?? '',
				was: scope.org ?? '',
				from: page.url.pathname
			})
		);
	}
</script>

<form method="GET" action="/switch" class="switcher" data-sveltekit-reload>
	<input type="hidden" name="from" value={page.url.pathname} />
	<input type="hidden" name="was" value={scope.org ?? ''} />
	<div class="field">
		<label for="switch-org">Organisation</label>
		<select id="switch-org" name="org" bind:this={orgSelect} value={scope.org ?? ''} onchange={go}>
			<option value="">{orgs.length > 0 ? 'None selected' : 'No organisations yet'}</option>
			{#if scope.org && !orgs.some((o) => o.id === scope.org)}
				<option value={scope.org}>{scope.org}</option>
			{/if}
			{#each orgs as org (org.id)}
				<option value={org.id}>{org.label}</option>
			{/each}
		</select>
	</div>
	<div class="field">
		<label for="switch-ws">Workspace</label>
		<select
			id="switch-ws"
			name="ws"
			bind:this={wsSelect}
			value={scope.ws ?? ''}
			disabled={!scope.org}
			onchange={go}
		>
			<option value="">
				{!scope.org
					? 'Choose an organisation first'
					: workspaces.length > 0
						? 'None selected'
						: 'No workspaces yet'}
			</option>
			{#if scope.ws && !workspaces.some((w) => w.id === scope.ws)}
				<option value={scope.ws}>{scope.ws}</option>
			{/if}
			{#each workspaces as ws (ws.id)}
				<option value={ws.id}>{ws.label}</option>
			{/each}
		</select>
	</div>
	<noscript><button type="submit" class="btn">Switch</button></noscript>
</form>

<style>
	.switcher {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	label {
		color: var(--text-muted);
		font-size: var(--text-xs);
		font-weight: 650;
		letter-spacing: 0.03em;
		text-transform: uppercase;
	}

	select {
		font-size: var(--text-sm);
	}
</style>
