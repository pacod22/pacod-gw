<script lang="ts">
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
</script>

<ResourcePage
	title="Rate classes"
	description="Throttles defined once and named by API keys: calls and tokens per minute, and calls in progress at once, for each key in the class. A key's own limits and its workspace's apply as well; the lowest wins."
	{data}
	{form}
	columns={[
		{ key: 'name', label: 'Name', kind: 'code' },
		{ key: 'description', label: 'Description' },
		{ key: 'requests_per_minute', label: 'Calls per minute', kind: 'number', empty: '–' },
		{ key: 'tokens_per_minute', label: 'Tokens per minute', kind: 'number', empty: '–' },
		{ key: 'max_concurrency', label: 'At once', kind: 'number', empty: '–' },
		{ key: 'status', label: 'Status', kind: 'status' }
	]}
	empty="No rate classes yet. Add one, then choose it on the API keys it should throttle."
	deleteNote="A rate class that keys name cannot be deleted; change the keys first."
/>
