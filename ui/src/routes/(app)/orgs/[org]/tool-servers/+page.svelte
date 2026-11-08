<script lang="ts">
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { organisationPath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const organisation = $derived(
		data.orgs.find((org) => org.id === params.org)?.label ?? params.org
	);
	const base = $derived(organisationPath(params.org, 'tool-servers'));
</script>

<ResourcePage
	title="Tool servers of {organisation}"
	description="MCP servers whose tools the organisation's callers reach through the gateway's /mcp, under the name server__tool. The gateway calls each with the organisation's credential, never with the caller's key, and serves only the tools an administrator approved, as approved: a server that changes a tool holds it until it is approved again."
	{data}
	{form}
	columns={[
		{ key: 'name', label: 'Name', kind: 'code', href: (s) => `${base}/${s.id}` },
		{ key: 'url', label: 'Address', kind: 'code' },
		{ key: 'description', label: 'Description', wrap: true },
		{
			key: 'workspaces',
			label: 'Workspaces',
			value: (s) => (s.workspaces ? `${s.workspaces.length} let in` : 'All')
		},
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'updated_at', label: 'Changed (UTC)', kind: 'datetime' }
	]}
	empty="This organisation has no tool server. Register an MCP server to offer its tools to the organisation's agents."
	deleteNote="Its tools leave /mcp at once, and its credentials and approvals are deleted with it."
/>
