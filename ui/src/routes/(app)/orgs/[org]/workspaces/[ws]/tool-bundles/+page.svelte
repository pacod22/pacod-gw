<script lang="ts">
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<ResourcePage
	title="Tool bundles"
	description="Which tools of the organisation's tool servers this workspace's keys may call on /mcp. Nothing is granted by default: a key names the bundles it may use, and /mcp/&lt;bundle&gt; serves one bundle alone, for agents that should see few tools. A tool must also be approved, and its server open to this workspace."
	{data}
	{form}
	columns={[
		{ key: 'name', label: 'Name', kind: 'code' },
		{ key: 'description', label: 'Description', wrap: true },
		{ key: 'allow', label: 'Grants', kind: 'list' },
		{ key: 'deny', label: 'Never grants', kind: 'list' },
		{ key: 'status', label: 'Status', kind: 'status' }
	]}
	empty="This workspace grants no tools. Add a bundle, then name it on the keys that may use it."
	deleteNote="A bundle that a key names cannot be deleted."
/>
