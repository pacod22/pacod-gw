<script lang="ts">
	import { page } from '$app/state';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { workspacePath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const organisation = $derived(
		data.orgs.find((org) => org.id === params.org)?.label ?? params.org
	);
</script>

<ResourcePage
	title="Workspaces of {organisation}"
	description="A workspace is the unit applications work in: one application, project, environment or team. It owns its API keys, its settings and its usage."
	{data}
	{form}
	createInitial={page.url.searchParams.has('new') ? {} : null}
	columns={[
		{
			key: 'display_name',
			label: 'Workspace',
			href: (ws) => workspacePath(params.org, ws.id),
			value: (ws) => ws.display_name || ws.name
		},
		{ key: 'name', label: 'Name', kind: 'code' },
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'time_zone', label: 'Time zone' },
		{ key: 'created_at', label: 'Created (UTC)', kind: 'datetime' }
	]}
	empty="This organisation has no workspaces yet. Add the first workspace, then create an API key in it."
>
	{#snippet rowActions(item)}
		<a class="btn btn-quiet" href={workspacePath(params.org, item.id, 'keys')}>
			API keys<span class="visually-hidden"> of {item.name}</span>
		</a>
		<a class="btn btn-quiet" href={workspacePath(params.org, item.id, 'settings')}>
			Settings<span class="visually-hidden"> of {item.name}</span>
		</a>
	{/snippet}
</ResourcePage>
