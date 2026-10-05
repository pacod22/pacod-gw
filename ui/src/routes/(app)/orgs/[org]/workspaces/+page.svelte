<script lang="ts">
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
	columns={[
		{ key: 'name', label: 'Name', kind: 'code', href: (ws) => workspacePath(params.org, ws.id) },
		{ key: 'display_name', label: 'Display name' },
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'time_zone', label: 'Time zone' },
		{ key: 'tag_keys', label: 'Tag keys', kind: 'list', empty: 'None' },
		{ key: 'created_at', label: 'Created (UTC)', kind: 'datetime' }
	]}
	empty="This organisation has no workspaces yet. Add the first workspace, then create an API key in it."
	deleteNote="A workspace can be deleted only when it has no API keys left."
>
	{#snippet rowActions(item)}
		<a class="btn btn-quiet" href={workspacePath(params.org, item.id)}>
			API keys<span class="visually-hidden"> of {item.name}</span>
		</a>
	{/snippet}
</ResourcePage>
