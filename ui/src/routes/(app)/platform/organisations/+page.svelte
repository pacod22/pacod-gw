<script lang="ts">
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { organisationPath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<ResourcePage
	title="Organisations"
	description="A customer or a large internal unit. An organisation groups workspaces and sets the defaults they inherit."
	{data}
	{form}
	columns={[
		{ key: 'name', label: 'Name', kind: 'code', href: (org) => organisationPath(org.id) },
		{ key: 'display_name', label: 'Display name' },
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'created_at', label: 'Created (UTC)', kind: 'datetime' }
	]}
	empty="No organisations yet. Add the first organisation, then create its workspaces."
	deleteNote="An organisation can be deleted only when it has no workspaces left."
>
	{#snippet rowActions(item)}
		<a class="btn btn-quiet" href={organisationPath(item.id)}>
			Workspaces<span class="visually-hidden"> of {item.name}</span>
		</a>
	{/snippet}
</ResourcePage>
