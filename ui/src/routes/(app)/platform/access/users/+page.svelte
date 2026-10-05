<script lang="ts">
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<ResourcePage
	title="Users"
	description="Accounts that sign in to this console. A user can do nothing until a role binding gives them a role."
	{data}
	{form}
	columns={[
		{ key: 'email', label: 'Email' },
		{ key: 'display_name', label: 'Display name' },
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'id', label: 'Id', kind: 'code' },
		{ key: 'created_at', label: 'Created (UTC)', kind: 'datetime' }
	]}
	empty="No users are visible to you."
	deleteNote="The user is signed out everywhere. Their role bindings should be deleted as well."
/>
