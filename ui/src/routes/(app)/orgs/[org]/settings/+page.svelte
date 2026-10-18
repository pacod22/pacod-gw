<script lang="ts">
	import OwnCredentials from '#lib/components/OwnCredentials.svelte';
	import SettingsPage from '#lib/components/SettingsPage.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const name = $derived(data.item?.display_name || data.item?.name || 'Organisation');
</script>

<SettingsPage
	title="Settings of {name}"
	description="The organisation’s name, the defaults its workspaces inherit, audit mode and the settings it locks for them."
	{data}
	{form}
	inside={data.inside}
	disableNote="A disabled organisation refuses every call to its workspaces until it is enabled again. Only platform admins switch it."
	lockedNote="Only platform admins change the name."
>
	{#snippet more()}
		<OwnCredentials
			noun="organisation"
			credentials={data.credentials}
			fields={data.credentialFields}
			copyFields={data.copyFields}
			mayManage={data.mayManageCredentials}
			{form}
		/>
	{/snippet}
</SettingsPage>
