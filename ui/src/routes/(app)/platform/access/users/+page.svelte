<script lang="ts">
	import { page } from '$app/state';
	import { describeRole } from '#lib/access.ts';
	import type { User } from '#lib/api/types.ts';
	import Notice from '#lib/components/Notice.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import RoleList from '#lib/components/RoleList.svelte';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	// Unknown when the role bindings could not be read; none when nothing binds them.
	const rolesOf = (user: User) => (data.roles ? (data.roles[user.id] ?? []) : undefined);
	const userHref = (user: User) => `/platform/access/users/${encodeURIComponent(user.id)}`;
	/** Set by the user's page after deleting. */
	const deleted = $derived(page.url.searchParams.get('deleted'));
</script>

<ResourcePage
	title="Users"
	description="Accounts that sign in to this console, and their roles. Open a user to change the account and its roles, send a password reset or delete it. A user can do nothing until they have a role."
	{data}
	form={null}
	columns={[
		{ key: 'email', label: 'Email', href: userHref },
		{ key: 'display_name', label: 'Display name' },
		{
			key: 'roles',
			label: 'Roles',
			value: (user) => (rolesOf(user) ?? []).map(describeRole).join(', ')
		},
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'email_verified', label: 'Confirmed', kind: 'bool' },
		{ key: 'created_at', label: 'Created (UTC)', kind: 'datetime' }
	]}
	custom={['roles']}
	empty="No users are visible to you."
	createHref="/platform/access/users/new"
	editHref={userHref}
	deletable={() => false}
>
	{#if deleted}<Notice tone="success"><p>The user {deleted} was deleted.</p></Notice>{/if}
	{#if data.rolesProblem}<ProblemNotice problem={data.rolesProblem} />{/if}
	{#snippet cell(item, column)}
		{#if column.key === 'roles'}<RoleList roles={rolesOf(item)} />{/if}
	{/snippet}
</ResourcePage>
