<script lang="ts">
	import Badge from '#lib/components/Badge.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import {
		countsText,
		formatDuration,
		runDuration,
		statusTone,
		type RunSummary
	} from '#lib/testing.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();
</script>

<PageHeader
	title="Test runs"
	description="The last hundred runs, newest first. A run's page shows every step with what was sent and what came back; secrets are masked."
/>

{#if data.problem}
	<ProblemNotice problem={data.problem} />
{:else if data.runs.length === 0}
	<div class="card muted">No run yet. <a href="/platform/testing">Start one</a>.</div>
{:else}
	<DataTable
		caption="Test runs"
		rows={data.runs}
		columns={[
			{
				key: 'created_at',
				label: 'Started (UTC)',
				kind: 'datetime',
				href: (r) => `/platform/testing/runs/${r.id}`
			},
			{ key: 'status', label: 'Result' },
			{ key: 'mode', label: 'Mode' },
			{ key: 'counts', label: 'Cases', value: (r) => countsText(r.counts), wrap: true },
			{
				key: 'duration',
				label: 'Took',
				kind: 'number',
				value: (r) => formatDuration(runDuration(r)),
				sortValue: (r) => runDuration(r)
			},
			{ key: 'started_by', label: 'Started by', value: (r) => r.started_by.display },
			{
				key: 'leftovers',
				label: 'Left over',
				kind: 'number',
				value: (r) => (r.leftovers ? String(r.leftovers) : '')
			}
		]}
		cell={status}
		custom={['status']}
	/>
{/if}

{#snippet status(run: RunSummary)}
	<Badge value={run.status} tone={statusTone(run.status)} />
{/snippet}
