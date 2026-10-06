<script lang="ts">
	import MembersPage from '#lib/components/MembersPage.svelte';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const workspace = $derived({
		id: params.ws,
		name: data.workspaces.find((w) => w.id === params.ws)?.name ?? 'the workspace'
	});
</script>

<MembersPage
	title="Members of {workspace.name}"
	description="Everyone with a role in this workspace, including the organisation’s roles, which apply here too."
	{data}
	{form}
	{workspace}
/>
