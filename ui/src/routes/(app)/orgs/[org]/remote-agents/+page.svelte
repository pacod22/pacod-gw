<script lang="ts">
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import { organisationPath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const organisation = $derived(
		data.orgs.find((org) => org.id === params.org)?.label ?? params.org
	);
	const base = $derived(organisationPath(params.org, 'remote-agents'));
</script>

<ResourcePage
	title="Remote agents of {organisation}"
	description="A2A agents the organisation's callers reach through the gateway at /a2a/<name>, granted by a tool bundle entry a2a:<name>. The gateway calls each with the organisation's credential, serves its card as approved and pointed at itself, and keeps each workspace's tasks apart."
	{data}
	{form}
	columns={[
		{ key: 'name', label: 'Name', kind: 'code', href: (a) => `${base}/${a.id}` },
		{ key: 'card_url', label: 'Card', kind: 'code' },
		{ key: 'description', label: 'Description', wrap: true },
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'updated_at', label: 'Changed (UTC)', kind: 'datetime' }
	]}
	empty="This organisation has no remote agent. Register an A2A agent by the address of its card."
	deleteNote="/a2a/<name> stops answering at once, and its credentials and approved card are deleted with it."
/>
