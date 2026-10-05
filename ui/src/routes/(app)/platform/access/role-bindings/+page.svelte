<script lang="ts">
	import type { RoleBinding } from '#lib/api/types.ts';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const who = (binding: RoleBinding) => data.principals[binding.principal] ?? binding.principal;
	const where = (binding: RoleBinding) =>
		binding.scope.type === 'platform' ? 'Platform' : `${binding.scope.type} ${binding.scope.id}`;
</script>

<ResourcePage
	title="Role bindings"
	description="A role binding gives a user or an admin token one role in one scope: the platform, an organisation or a workspace."
	{data}
	{form}
	nameOf={(binding) => `${binding.role.replaceAll('_', ' ')} for ${who(binding)}`}
	columns={[
		{ key: 'principal', label: 'User or token', value: who },
		{ key: 'role', label: 'Role', value: (binding) => binding.role.replaceAll('_', ' ') },
		{ key: 'scope', label: 'Scope', value: where },
		{ key: 'created_at', label: 'Created (UTC)', kind: 'datetime' }
	]}
	empty="No role bindings are visible to you. Add one to give a user or an admin token a role."
	deleteNote="The role is taken away at once. The last platform admin binding cannot be deleted."
/>
