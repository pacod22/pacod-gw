<script lang="ts">
	import { enhance } from '$app/forms';
	import CopyButton from '#lib/components/CopyButton.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<ResourcePage
	title="Users"
	description="Accounts that sign in to this console. A user can do nothing until a role binding gives them a role. When the installation sends email, a new user confirms the address before the first sign-in."
	{data}
	{form}
	columns={[
		{ key: 'email', label: 'Email' },
		{ key: 'display_name', label: 'Display name' },
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'email_verified', label: 'Confirmed', kind: 'bool' },
		{ key: 'created_at', label: 'Created (UTC)', kind: 'datetime' }
	]}
	empty="No users are visible to you."
	deleteNote="The user is signed out everywhere, and their roles are taken away."
	pageActions={['verification', 'reset', 'confirm']}
>
	{#snippet rowActions(item)}
		<CopyButton value={item.id} label="Copy id" what="of {item.email}" />
		{#if !item.email_verified}
			<form method="POST" action="?/verification" use:enhance>
				<input type="hidden" name="id" value={item.id} />
				<input type="hidden" name="email" value={item.email} />
				<button type="submit" class="btn btn-quiet">
					Send verification<span class="visually-hidden"> to {item.email}</span>
				</button>
			</form>
			<form method="POST" action="?/confirm" use:enhance>
				<input type="hidden" name="id" value={item.id} />
				<input type="hidden" name="email" value={item.email} />
				<button type="submit" class="btn btn-quiet">
					Mark confirmed<span class="visually-hidden"> {item.email}</span>
				</button>
			</form>
		{/if}
		{#if item.status === 'active'}
			<form method="POST" action="?/reset" use:enhance>
				<input type="hidden" name="id" value={item.id} />
				<input type="hidden" name="email" value={item.email} />
				<button type="submit" class="btn btn-quiet">
					Send password reset<span class="visually-hidden"> to {item.email}</span>
				</button>
			</form>
		{/if}
	{/snippet}
</ResourcePage>
