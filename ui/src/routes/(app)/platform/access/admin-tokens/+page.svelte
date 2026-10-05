<script lang="ts">
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<ResourcePage
	title="Admin tokens"
	description="Bearer tokens for automation that calls the management API. A token can do nothing until a role binding gives it a role. The token is shown once, when it is created."
	{data}
	{form}
	columns={[
		{ key: 'name', label: 'Name' },
		{ key: 'hint', label: 'Ends in', kind: 'code', value: (token) => `…${token.hint}` },
		{ key: 'status', label: 'Status', kind: 'status' },
		{ key: 'expires_at', label: 'Expires (UTC)', kind: 'datetime', empty: 'Never' },
		{ key: 'id', label: 'Id', kind: 'code' },
		{ key: 'created_at', label: 'Created (UTC)', kind: 'datetime' }
	]}
	empty="No admin tokens yet. Add one for a script or a pipeline, then bind a role to it."
	deleteNote="Automation that uses this token is refused at once."
/>
