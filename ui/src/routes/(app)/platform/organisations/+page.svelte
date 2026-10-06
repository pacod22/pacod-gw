<script lang="ts">
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { organisationPath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<ResourcePage
	title="All organisations"
	description="A customer or a large internal unit. An organisation groups workspaces and sets the defaults they inherit; its admins run it from its own pages."
	{data}
	{form}
	columns={[
		{
			key: 'display_name',
			label: 'Organisation',
			href: (org) => organisationPath(org.id),
			value: (org) => org.display_name || org.name
		},
		{ key: 'name', label: 'Name', kind: 'code' },
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'created_at', label: 'Created (UTC)', kind: 'datetime' }
	]}
	empty="No organisations yet. Add the first organisation and invite its admin."
>
	{#snippet rowActions(item)}
		<a class="btn btn-quiet" href={organisationPath(item.id, 'members')}>
			Members<span class="visually-hidden"> of {item.name}</span>
		</a>
		<a class="btn btn-quiet" href={organisationPath(item.id, 'settings')}>
			Settings<span class="visually-hidden"> of {item.name}</span>
		</a>
	{/snippet}
</ResourcePage>
