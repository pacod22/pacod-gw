<script lang="ts">
	import { untrack } from 'svelte';
	import type { ScopeNames } from '#lib/server/options.ts';
	import type { ActionOutcome } from '#lib/server/resource.ts';
	import type { HeldRole } from '#lib/server/roles.ts';
	import ActionDialog from './ActionDialog.svelte';
	import RolesEditor from './RolesEditor.svelte';

	/** The roles of one user or admin token in a dialog, sent to the `roles` action. */
	interface Props {
		open: boolean;
		/** The user or admin token. */
		who: { id: string; name: string };
		roles: HeldRole[];
		scopes: ScopeNames;
		outcome: ActionOutcome | null | undefined;
	}

	let { open = $bindable(false), who, roles, scopes, outcome }: Props = $props();

	let ready = $state(true);
	/** Counts openings: each one starts from the roles as they are. */
	let generation = $state(0);
	$effect(() => {
		if (open) untrack(() => (generation += 1));
	});
</script>

<ActionDialog
	bind:open
	title="Roles of {who.name}"
	action="?/roles"
	submit="Save"
	{outcome}
	hidden={{ principal: who.id, name: who.name }}
	{ready}
>
	{#key generation}
		<RolesEditor name={who.name} {roles} {scopes} bind:ready />
	{/key}
</ActionDialog>
