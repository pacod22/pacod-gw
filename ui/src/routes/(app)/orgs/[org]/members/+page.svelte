<script lang="ts">
	import MembersPage from '#lib/components/MembersPage.svelte';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const organisation = $derived(
		data.orgs.find((org) => org.id === params.org)?.label ?? 'the organisation'
	);
</script>

<MembersPage
	title="Members of {organisation}"
	description="Everyone with a role in the organisation or one of its workspaces. An organisation role applies to all its workspaces; a workspace role to that workspace only."
	{data}
	{form}
/>
