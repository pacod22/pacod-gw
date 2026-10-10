<script lang="ts">
	import ObjectsPage from '#lib/components/ObjectsPage.svelte';
	import { formatCost } from '#lib/format.ts';
	import type { PageProps } from './$types';

	let { data, params }: PageProps = $props();

	const workspace = $derived(data.workspaces.find((w) => w.id === params.ws)?.label ?? params.ws);
	/** All items of a batch, whatever became of them. */
	const requests = (o: { object: unknown }) => {
		const counts = (o.object as { request_counts?: Record<string, number> } | null)?.request_counts;
		return counts ? Object.values(counts).reduce((a, b) => a + b, 0) : null;
	};
</script>

<ObjectsPage
	org={params.org}
	ws={params.ws}
	{workspace}
	kind="batches"
	{data}
	columns={[
		{ key: 'id', label: 'Id', kind: 'code' },
		{ key: 'requests', label: 'Requests', kind: 'number', value: requests },
		{
			key: 'cost',
			label: 'Cost',
			kind: 'number',
			value: (o) => formatCost(o.cost, o.currency),
			sortValue: (o) => o.cost,
			empty: 'Not settled yet'
		},
		{ key: 'settled_at', label: 'Settled (UTC)', kind: 'datetime' }
	]}
	empty="This workspace has not created a message batch through the gateway. Batches are created at /v1/messages/batches; each item is checked and limited like a single message, and accounted when the batch has ended."
/>
