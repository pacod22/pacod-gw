<script lang="ts">
	import { untrack } from 'svelte';
	import { describeRole, roleLabel } from '#lib/access.ts';
	import type { RoleScope } from '#lib/api/types.ts';
	import { ROLE_HELP, rolesFor, type RoleName } from '#lib/members.ts';
	import type { ScopeNames } from '#lib/server/options.ts';
	import type { HeldRole } from '#lib/server/roles.ts';

	/**
	 * The role controls inside a form: the roles someone has, each with a box to clear to take
	 * it away, and pickers to give new ones. What it sends is read by `applyRoles` in
	 * `#lib/server/roles.ts`. The surrounding form and its Save button belong to the caller.
	 */
	interface Props {
		/** How the user or admin token is called in the hints. */
		name: string;
		roles: HeldRole[];
		scopes: ScopeNames;
		/** False while the pickers ask for a role they have already: the form should wait. */
		ready?: boolean;
	}

	let { name, roles, scopes, ready = $bindable() }: Props = $props();

	const uid = $props.id();
	let place = $state('');
	let role = $state('');
	let typedWorkspace = $state('');
	/** New roles queued with "Add another", before the form is saved. */
	let queued: { role: RoleName; place: string; label: string }[] = $state([]);

	function scopeOf(value: string): RoleScope | null {
		if (value === 'platform') return { type: 'platform' };
		const [type, ...rest] = value.split(':');
		const id = rest.join(':') || (type === 'workspace' ? typedWorkspace.trim() : '');
		if (type === 'organisation' || type === 'workspace') return { type, id };
		return null;
	}

	const scope = $derived(scopeOf(place));
	const choices = $derived(scope ? rolesFor(scope.type) : []);
	$effect(() => {
		// A role that does not fit the place is dropped.
		if (!choices.includes(role as RoleName)) untrack(() => (role = ''));
	});

	/** The place as sent: a typed workspace id is written into it. */
	const placeValue = $derived(
		scope && scope.type === 'workspace' ? `workspace:${scope.id}` : place
	);

	function label(r: RoleName, s: RoleScope): string {
		const ws = s.type === 'workspace' ? scopes.workspaces?.find((w) => w.id === s.id) : undefined;
		const org =
			s.type === 'organisation' ? scopes.organisations.find((o) => o.id === s.id) : undefined;
		return describeRole({
			role: r,
			scope: s,
			scope_name: org?.name ?? ws?.name ?? ('id' in s ? s.id : null),
			organisation_name: ws?.organisationName ?? null
		});
	}

	const chosen = $derived(scope && role ? label(role as RoleName, scope) : '');
	/** `viewer|organisation:<id>`: a role in a place, for comparing. */
	const keyOf = (r: string, s: RoleScope) => `${r}|${'id' in s ? `${s.type}:${s.id}` : s.type}`;
	/** The chosen role is one they have or one already queued: the core would refuse it. */
	const duplicate = $derived.by(() => {
		if (!scope || !role) return false;
		const key = keyOf(role, scope);
		return (
			roles.some((r) => keyOf(r.role, r.scope) === key) ||
			queued.some((q) => `${q.role}|${q.place}` === key)
		);
	});
	$effect(() => {
		if (ready !== !duplicate) ready = !duplicate;
	});

	function queue() {
		if (!scope || !role || duplicate) return;
		queued.push({ role: role as RoleName, place: placeValue, label: chosen });
		place = '';
		role = '';
		typedWorkspace = '';
	}
</script>

<fieldset class="group">
	<legend>Current roles</legend>
	{#if roles.length === 0}
		<p class="muted small">None yet: {name} can do nothing until they have a role.</p>
	{:else}
		<p class="muted small">Clear a box to take the role away.</p>
		{#each roles as held (held.binding_id)}
			<input type="hidden" name="held" value={held.binding_id} />
			<input type="hidden" name="label:{held.binding_id}" value={describeRole(held)} />
			<label class="check">
				<input type="checkbox" name="keep" value={held.binding_id} checked />
				<span>{describeRole(held)}</span>
			</label>
		{/each}
	{/if}
</fieldset>

<fieldset class="group">
	<legend>Give a role</legend>
	{#if queued.length > 0}
		<ul class="queued">
			{#each queued as item, i (`${item.role}|${item.place}`)}
				<li>
					<input type="hidden" name="add" value="{item.role}|{item.place}|{item.label}" />
					<span>{item.label}</span>
					<span class="muted small">new</span>
					<button type="button" class="btn btn-quiet" onclick={() => queued.splice(i, 1)}>
						Remove<span class="visually-hidden"> {item.label}</span>
					</button>
				</li>
			{/each}
		</ul>
	{/if}
	<div class="grid">
		<label for="{uid}-place">Where</label>
		<select id="{uid}-place" name="place" bind:value={place}>
			<option value="">{queued.length > 0 ? 'No further role' : 'No new role'}</option>
			<option value="platform">The platform</option>
			{#if scopes.organisations.length > 0}
				<optgroup label="Organisations">
					{#each scopes.organisations as org (org.id)}
						<option value="organisation:{org.id}">{org.name}</option>
					{/each}
				</optgroup>
			{/if}
			{#if scopes.workspaces === null}
				<option value="workspace:">A workspace, by id</option>
			{:else if scopes.workspaces.length > 0}
				<optgroup label="Workspaces">
					{#each scopes.workspaces as ws (ws.id)}
						<option value="workspace:{ws.id}">{ws.organisationName} / {ws.name}</option>
					{/each}
				</optgroup>
			{/if}
		</select>

		{#if place === 'workspace:'}
			<label for="{uid}-workspace">Workspace id</label>
			<input
				id="{uid}-workspace"
				class="mono"
				name="workspace"
				required
				bind:value={typedWorkspace}
			/>
		{/if}

		<label for="{uid}-role">Role</label>
		<select
			id="{uid}-role"
			name="role"
			bind:value={role}
			disabled={scope === null}
			required={scope !== null}
		>
			<option value="">{scope ? 'Choose a role' : 'Choose where first'}</option>
			{#each choices as choice (choice)}
				<option value={choice}>{roleLabel(choice)}</option>
			{/each}
		</select>
	</div>
	<input type="hidden" name="added" value={chosen} />
	{#if duplicate}
		<p class="small warning-text">{name} has this role already.</p>
	{:else if role}
		<p class="muted small">{ROLE_HELP[role]}</p>
	{/if}
	<div>
		<button type="button" class="btn" disabled={!chosen || duplicate} onclick={queue}>
			Add another role
		</button>
	</div>
</fieldset>

<style>
	.group {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		padding: 0;
		border: 0;
	}

	.group legend {
		margin-bottom: var(--space-2);
		padding: 0;
		font-weight: 600;
	}

	.check {
		display: flex;
		align-items: flex-start;
		gap: var(--space-2);
		cursor: pointer;
	}

	.queued {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		padding: 0;
		list-style: none;
	}

	.queued li {
		display: flex;
		align-items: center;
		gap: var(--space-2);
	}

	.grid {
		display: grid;
		grid-template-columns: minmax(6rem, auto) 1fr;
		align-items: center;
		gap: var(--space-2) var(--space-3);
	}

	.grid select,
	.grid input {
		min-width: 0;
	}

	.warning-text {
		color: var(--warning);
	}
</style>
