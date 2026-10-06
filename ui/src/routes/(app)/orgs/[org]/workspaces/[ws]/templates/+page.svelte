<script lang="ts">
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { organisationPath, workspacePath } from '#lib/nav.ts';
	import { describeModels } from '#lib/templates.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const workspace = $derived(data.workspaces.find((ws) => ws.id === params.ws)?.label ?? params.ws);
	const organisation = $derived(
		data.orgs.find((org) => org.id === params.org)?.label ?? params.org
	);
	const base = $derived(workspacePath(params.org, params.ws, 'templates'));
</script>

<ResourcePage
	title="Templates of {workspace}"
	description="A template fixes the model and the parameters of a kind of call, so that applications name the template instead of carrying the settings. Every save is a new version; an earlier one can be made current again."
	crumbs={[{ label: organisation, href: organisationPath(params.org) }]}
	{data}
	{form}
	columns={[
		{
			key: 'name',
			label: 'Name',
			kind: 'code',
			value: (t) => `@${t.name}`,
			href: (t) => `${base}/${t.id}`
		},
		{ key: 'description', label: 'Description', wrap: true },
		{
			key: 'model',
			label: 'Models',
			kind: 'code',
			value: (t) => describeModels(t.spec),
			wrap: true
		},
		{ key: 'current_version', label: 'Version', kind: 'number' },
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'updated_at', label: 'Changed (UTC)', kind: 'datetime' }
	]}
	empty="This workspace has no templates yet. Add one for the first kind of call your application makes."
	deleteNote="Calls that name this template are rejected. A template that an API key's allow-list names cannot be deleted until the key no longer names it."
	createHref="{base}/new"
	editHref={(template) => `${base}/${template.id}`}
/>
