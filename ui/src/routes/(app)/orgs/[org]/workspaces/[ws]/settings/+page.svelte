<script lang="ts">
	import OwnCredentials from '#lib/components/OwnCredentials.svelte';
	import SettingsPage from '#lib/components/SettingsPage.svelte';
	import type { PageProps } from './$types';

	let { data, form, params }: PageProps = $props();

	const name = $derived(data.item?.display_name || data.item?.name || params.ws);
</script>

<SettingsPage
	title="Settings of {name}"
	description="The workspace’s name, time zone and tag keys, how it handles requests, and audit mode. What it does not set is inherited from the organisation, then from the installation."
	{data}
	{form}
	inside={data.inside}
	disableNote="A disabled workspace refuses every call made with its API keys until it is enabled again."
	lockedNote="Locked by the organisation: it decides these for all its workspaces."
>
	{#snippet more()}
		<OwnCredentials
			noun="workspace"
			credentials={data.credentials}
			fields={data.credentialFields}
			copyFields={data.copyFields}
			mayManage={data.mayManageCredentials}
			{form}
		/>
	{/snippet}
</SettingsPage>
