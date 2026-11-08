<script lang="ts">
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<ResourcePage
	title="Agents"
	description="The workspace's registered agents. A registered agent is spoken for only by its own keys (an API key's agents): its calls carry its name whatever they say, are held to what it may use within its keys, and stop when it is disabled. Until an agent is registered, its name is only a label any key of the workspace can send in x-gw-agent-id."
	{data}
	{form}
	columns={[
		{ key: 'name', label: 'Name', kind: 'code' },
		{ key: 'display_name', label: 'Shown as' },
		{ key: 'description', label: 'Description', wrap: true },
		{ key: 'allowed_models', label: 'Models', kind: 'list' },
		{ key: 'tool_bundles', label: 'Tool bundles', kind: 'list' },
		{ key: 'status', label: 'Status', kind: 'status' }
	]}
	empty="No agent is registered. Register one, then give it a key: its calls can then be told apart, limited and stopped."
	deleteNote="An agent that a key speaks for cannot be deleted. Disable it to stop it."
/>
