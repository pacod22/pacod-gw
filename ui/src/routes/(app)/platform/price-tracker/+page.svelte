<script lang="ts">
	import { enhance } from '$app/forms';
	import type { Schemas } from '#lib/api/types.ts';
	import ActionDialog from '#lib/components/ActionDialog.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import PriceRates from '#lib/components/PriceRates.svelte';
	import ObjectPage from '#lib/components/ObjectPage.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import { formatDateTime } from '#lib/format.ts';
	import { rateText } from '#lib/prices.ts';
	import type { Row } from '#lib/report/format.ts';
	import type { PageProps } from './$types';

	type Proposal = Schemas['Item14'];

	let { data, form }: PageProps = $props();

	const view = $derived(data.item);
	const writable = $derived(data.fields !== null);
	let importing = $state(false);
	const imported = $derived(
		form?.ok && form.action === 'import' && Array.isArray(form.data) ? (form.data as Row[]) : null
	);
	const runs = $derived(
		data.runs.map((run) => ({
			started_at: run.started_at,
			trigger: run.trigger,
			result:
				run.ok === null || run.ok === undefined ? 'in progress' : run.ok ? 'finished' : 'failed',
			outcomes: Object.entries(
				((run.result ?? {}) as { counts?: Record<string, unknown> }).counts ?? {}
			)
				.map(([outcome, count]) => `${count} ${outcome.replaceAll('_', ' ')}`)
				.join(', '),
			message: run.message,
			id: run.id
		}))
	);
</script>

<ObjectPage
	title="Price tracker"
	description="A scheduled job that reads the prices of the providers and keeps the model prices current. Small changes are applied; a change above the bound waits as a proposal for approval."
	{data}
	{form}
>
	{#if view}
		<section class="card stack">
			<h2>State</h2>
			<dl class="facts">
				<dt>Job</dt>
				<dd>
					{view.enabled ? 'Switched on' : 'Switched off'}{view.running
						? ', running now'
						: view.run_requested
							? ', a run was requested'
							: ''}
				</dd>
				<dt>Last started (UTC)</dt>
				<dd>{view.last_started_at ? formatDateTime(view.last_started_at, true) : 'Never'}</dd>
				<dt>Next run (UTC)</dt>
				<dd>
					{view.next_run_at ? formatDateTime(view.next_run_at) : 'None: the job is switched off'}
				</dd>
			</dl>
			{#if writable}
				<div class="row">
					<form method="POST" action="?/run" use:enhance>
						<button type="submit" class="btn" disabled={view.running || view.run_requested}
							>Run now</button
						>
					</form>
					<button type="button" class="btn" onclick={() => (importing = true)}>Import prices</button
					>
				</div>
			{/if}
		</section>
		<h2>Schedule and sources</h2>
	{/if}
</ObjectPage>

{#if imported}
	<section class="stack after">
		<h2>Result of the import</h2>
		<ReportTable
			caption="Result of the import"
			rows={imported}
			key="model"
			empty="The list held no prices."
		/>
	</section>
{/if}

<section class="stack after">
	<h2>Proposals waiting for approval</h2>
	{#if data.proposalsProblem}
		<ProblemNotice problem={data.proposalsProblem} />
	{:else if data.proposals.length === 0}
		<div class="card muted">No proposal is waiting.</div>
	{:else}
		<DataTable
			caption="Price proposals"
			rows={data.proposals}
			columns={[
				{
					key: 'model',
					label: 'Model',
					kind: 'code',
					href: (p) => `/platform/models/${p.model_id}/prices`
				},
				{
					key: 'rates',
					label: 'Proposed rates / 1M',
					value: rateText,
					sortValue: (p) => p.input ?? null
				},
				{
					key: 'source',
					label: 'Source, effective from (UTC)',
					sortValue: (p) => p.effective_from ?? null
				},
				{
					key: 'evidence_text',
					label: 'Evidence',
					wrap: true,
					value: (p) => p.evidence_text || p.evidence_url
				}
			]}
			actions={writable ? decide : undefined}
			cell={proposedRates}
			custom={['rates', 'source']}
		/>
	{/if}
</section>

{#snippet proposedRates(proposal: Proposal, column: { key: string })}
	{#if column.key === 'rates'}
		<PriceRates price={proposal} currency={proposal.currency} />
	{:else}
		<span class="stacked">
			<span>{proposal.source}</span>
			<span class="muted small nowrap">{formatDateTime(proposal.effective_from)}</span>
		</span>
	{/if}
{/snippet}

{#snippet decide(proposal: Proposal)}
	<form method="POST" action="?/approve" use:enhance>
		<input type="hidden" name="id" value={proposal.id} />
		<button type="submit" class="btn btn-quiet"
			>Approve<span class="visually-hidden"> the price of {proposal.model}</span></button
		>
	</form>
	<form method="POST" action="?/reject" use:enhance>
		<input type="hidden" name="id" value={proposal.id} />
		<button type="submit" class="btn btn-quiet danger"
			>Reject<span class="visually-hidden"> the price of {proposal.model}</span></button
		>
	</form>
{/snippet}

<section class="stack after">
	<h2>Runs</h2>
	{#if data.runsProblem}
		<ProblemNotice problem={data.runsProblem} />
	{:else}
		<ReportTable
			caption="Runs of the price job"
			rows={runs}
			key="id"
			fields={['started_at', 'trigger', 'result', 'outcomes', 'message']}
			labels={{ started_at: 'Started (UTC)' }}
			empty="The job has not run yet."
		/>
	{/if}
</section>

{#if data.importFields}
	<ActionDialog
		bind:open={importing}
		title="Import prices"
		action="?/import"
		submit="Import prices"
		outcome={form}
		fields={data.importFields}
	>
		<p class="muted small">
			Each price becomes a new version of its model's price, in effect at once.
		</p>
	</ActionDialog>
{/if}

<style>
	.stacked {
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.nowrap {
		white-space: nowrap;
	}

	.after {
		margin-top: var(--space-5);
	}
</style>
