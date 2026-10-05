<script lang="ts">
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportFilters from '#lib/components/ReportFilters.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import { rowsOf } from '#lib/report/format.ts';
	import { page } from '$app/state';
	import { offsetLink, scopeQuery } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const rows = $derived(rowsOf(data.report.data, 'rows'));
	const limit = $derived(Number(data.report.filters.limit ?? 50) || 50);
	const offset = $derived(Number(data.report.filters.offset ?? 0) || 0);
	const link = (to: number) => offsetLink(page.url, to);
</script>

<div class="stack-lg">
	<PageHeader
		title="End users"
		description="The people or accounts calls were made for, the most expensive first. Calls without an end user are not listed."
	/>
	<ReportFilters
		values={data.report.filters}
		controls={[
			{ name: 'from', label: 'From (UTC)', type: 'time' },
			{ name: 'to', label: 'To (UTC)', type: 'time' }
		]}
	/>
	{#if data.report.problem}
		<ProblemNotice problem={data.report.problem} />
	{:else}
		<ReportTable
			caption="End users"
			{rows}
			key="end_user"
			href={(row) =>
				`/insight/end-users/${encodeURIComponent(String(row.end_user))}${scopeQuery(data.scope)}`}
			empty={offset > 0
				? 'There are no further end users.'
				: 'No call in this range named an end user.'}
		/>
		{#if offset > 0 || rows.length === limit}
			<nav class="pager" aria-label="Pages">
				<span class="muted small">
					{rows.length > 0
						? `End users ${offset + 1} to ${offset + rows.length}`
						: 'No further end users'}
				</span>
				<div class="row">
					{#if offset > 0}<a class="btn" href={link(Math.max(0, offset - limit))}>Previous page</a
						>{/if}
					{#if rows.length === limit}<a class="btn" href={link(offset + limit)}>Next page</a>{/if}
				</div>
			</nav>
		{/if}
	{/if}
</div>

<style>
	.pager {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-3);
	}
</style>
