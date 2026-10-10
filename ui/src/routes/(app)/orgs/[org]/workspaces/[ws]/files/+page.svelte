<script lang="ts">
	import ObjectsPage from '#lib/components/ObjectsPage.svelte';
	import type { PageProps } from './$types';

	let { data, params }: PageProps = $props();

	const workspace = $derived(data.workspaces.find((w) => w.id === params.ws)?.label ?? params.ws);
	const field = (o: { object: unknown }, name: string) =>
		(o.object as Record<string, unknown> | null)?.[name];
</script>

<ObjectsPage
	org={params.org}
	ws={params.ws}
	{workspace}
	kind="files"
	{data}
	columns={[
		{ key: 'filename', label: 'File', value: (o) => field(o, 'filename') },
		{ key: 'id', label: 'Id', kind: 'code' },
		{ key: 'size_bytes', label: 'Bytes', kind: 'number', value: (o) => field(o, 'size_bytes') }
	]}
	empty="This workspace has not uploaded a file through the gateway. Files are uploaded at /v1/files with Anthropic's version header."
/>
