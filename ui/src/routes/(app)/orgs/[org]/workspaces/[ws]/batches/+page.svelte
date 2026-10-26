<script lang="ts">
	import ObjectsPage from '#lib/components/ObjectsPage.svelte';
	import { formatCost } from '#lib/format.ts';
	import type { PageProps } from './$types';

	let { data, params }: PageProps = $props();

	const workspace = $derived(data.workspaces.find((w) => w.id === params.ws)?.label ?? params.ws);
	/**
	 * All items of a batch, whatever became of them: OpenAI counts them in `total`, Anthropic
	 * by what became of each.
	 */
	const requests = (o: { object: unknown }) => {
		const counts = (o.object as { request_counts?: Record<string, number> } | null)?.request_counts;
		if (!counts) return null;
		if (typeof counts.total === 'number') return counts.total;
		return Object.values(counts).reduce((a, b) => a + b, 0);
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
	empty="This workspace has not created a batch through the gateway. Batches are created at /v1/messages/batches (Anthropic) or /v1/batches (OpenAI); each item is checked and limited like a single call, and accounted when the batch has ended."
/>
