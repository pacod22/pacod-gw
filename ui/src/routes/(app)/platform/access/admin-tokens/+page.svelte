<script lang="ts">
	import { describeRole } from '#lib/access.ts';
	import type { AdminToken } from '#lib/api/types.ts';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import RoleList from '#lib/components/RoleList.svelte';
	import RolesDialog from '#lib/components/RolesDialog.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	// Unknown when the role bindings could not be read; none when nothing binds them.
	const rolesOf = (token: AdminToken) => (data.roles ? (data.roles[token.id] ?? []) : undefined);
	let editing: AdminToken | null = $state(null);
	let rolesOpen = $state(false);
</script>

<ResourcePage
	title="Admin tokens"
	description="Bearer tokens for automation that calls the management API, and their roles. A token can do nothing until it has a role: give one with Edit roles. The token is shown once, when it is created."
	{data}
	{form}
	columns={[
		{ key: 'name', label: 'Name' },
		{ key: 'hint', label: 'Ends in', kind: 'code', value: (token) => `…${token.hint}` },
		{
			key: 'roles',
			label: 'Roles',
			value: (token) => (rolesOf(token) ?? []).map(describeRole).join(', ')
		},
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'expires_at', label: 'Expires (UTC)', kind: 'datetime', empty: 'Never' },
		{ key: 'created_at', label: 'Created (UTC)', kind: 'datetime' }
	]}
	custom={['roles']}
	empty="No admin tokens yet. Add one for a script or a pipeline, then give it a role."
	deleteNote="Automation that uses this token is refused at once."
>
	{#if data.rolesProblem}<ProblemNotice problem={data.rolesProblem} />{/if}
	{#snippet cell(item, column)}
		{#if column.key === 'roles'}<RoleList roles={rolesOf(item)} />{/if}
	{/snippet}
	{#snippet rowActions(item)}
		{#if data.canManageRoles && data.roles}
			<button
				type="button"
				class="btn btn-quiet"
				onclick={() => {
					editing = item;
					rolesOpen = true;
				}}
			>
				Edit roles<span class="visually-hidden"> of {item.name}</span>
			</button>
		{/if}
		<CopyButton value={item.id} label="Copy id" what="of {item.name}" />
	{/snippet}
</ResourcePage>

{#if editing}
	<RolesDialog
		bind:open={rolesOpen}
		who={{ id: editing.id, name: editing.name }}
		roles={rolesOf(editing) ?? []}
		scopes={data.scopes}
		outcome={form}
	/>
{/if}
