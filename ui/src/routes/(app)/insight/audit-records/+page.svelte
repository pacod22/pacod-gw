<script lang="ts">
	import ActionDialog from '#lib/components/ActionDialog.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import Pager from '#lib/components/Pager.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportFilters from '#lib/components/ReportFilters.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import { rowsOf } from '#lib/report/format.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const rows = $derived(rowsOf(data.report.data, 'records'));
	const next = $derived(
		typeof data.report.data?.next_cursor === 'string' ? data.report.data.next_cursor : null
	);
	let deleting = $state(false);
</script>

<div class="stack-lg">
	<PageHeader
		title="Audit records"
		description="Calls that audit mode recorded in full, newest first. A record holds prompts and answers; opening one is noted in the audit log."
	>
		{#snippet actions()}
			{#if data.mayDelete && data.scope.ws}
				<button type="button" class="btn btn-danger" onclick={() => (deleting = true)}
					>Delete records</button
				>
			{/if}
		{/snippet}
	</PageHeader>
	<OutcomeNotice outcome={form} />
	<ReportFilters
		values={data.report.filters}
		controls={[
			{ name: 'from', label: 'From (UTC)', type: 'time' },
			{ name: 'to', label: 'To (UTC)', type: 'time' },
			{ name: 'end_user', label: 'End user' },
			{ name: 'session_id', label: 'Session id' },
			{ name: 'trace_id', label: 'Trace id' },
			{ name: 'agent_id', label: 'Agent' },
			{ name: 'task', label: 'Task' },
			{ name: 'template', label: 'Template' },
			{ name: 'model', label: 'Model' }
		]}
	/>
	{#if data.report.problem}
		<ProblemNotice problem={data.report.problem} />
	{:else}
		<ReportTable
			caption="Audit records"
			{rows}
			key="audit_id"
			fields={[
				'timestamp',
				'template',
				'model',
				'mode',
				'end_user',
				'agent_id',
				'task',
				'session_id',
				'trace_id',
				'expires_at',
				'truncated'
			]}
			labels={{ truncated: 'Cut at the size limit' }}
			href={(row) => `/insight/requests/${encodeURIComponent(String(row.event_id))}/audit`}
			empty="No call was recorded in full in this range. Audit mode is switched on in the settings of a workspace or on a template."
		/>
		<Pager cursor={data.cursor} nextCursor={next} count={rows.length} />
	{/if}
	{#if data.mayDelete && !data.scope.ws}
		<p class="muted small">
			To delete records, choose the workspace they belong to in the switcher.
		</p>
	{/if}
</div>

<ActionDialog
	bind:open={deleting}
	title="Delete audit records"
	action="?/delete"
	submit="Delete records"
	outcome={form}
	hidden={{ workspace_id: data.scope.ws ?? undefined }}
	danger
>
	<p>
		Delete the audit records of one end user or of one session in this workspace. They cannot be
		restored. Usage and cost figures are not affected.
	</p>
	<div class="field">
		<label for="del-user">End user</label>
		<input id="del-user" name="end_user" autocomplete="off" />
	</div>
	<div class="field">
		<label for="del-session">Session id</label>
		<input id="del-session" name="session_id" autocomplete="off" />
	</div>
	<p class="muted small">Name at least one of the two.</p>
</ActionDialog>

<style>
	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}
</style>
