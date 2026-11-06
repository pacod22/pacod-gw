<script lang="ts">
	import { enhance } from '$app/forms';
	import type { Schemas } from '#lib/api/types.ts';
	import ActionDialog from '#lib/components/ActionDialog.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import PriceRates from '#lib/components/PriceRates.svelte';
	import ObjectPage from '#lib/components/ObjectPage.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import { formatDateTime } from '#lib/format.ts';
	import { PRICING_TABS } from '#lib/nav.ts';
	import { rateText, runSummary } from '#lib/prices.ts';
	import type { Row } from '#lib/report/format.ts';
	import type { PageProps } from './$types';

	type Proposal = Schemas['Item14'];
	type JobView = Schemas['JobView'];

	let { data, form }: PageProps = $props();

	const view = $derived(data.item);
	const writable = $derived(data.fields !== null);
	let importing = $state(false);
	const imported = $derived(
		form?.ok && form.action === 'import' && Array.isArray(form.data) ? (form.data as Row[]) : null
	);
	const JOBS: Record<string, string> = {
		price_check: 'Check',
		price_scan: 'Scan',
		prices: 'Import or earlier job'
	};
	const runs = $derived(
		data.runs.map((run) => ({
			started_at: run.started_at,
			job: JOBS[run.job] ?? run.job,
			trigger: run.trigger,
			result:
				run.ok === null || run.ok === undefined ? 'in progress' : run.ok ? 'finished' : 'failed',
			outcomes: runSummary(run.result),
			message: run.message,
			id: run.id
		}))
	);

	/** "Running now", "asked for", or when it runs next. */
	function stateOf(job: JobView, enabled: boolean): string {
		if (job.running) return 'Running now';
		if (job.run_requested) return 'Asked for: it starts within a minute';
		if (!enabled) return 'Off';
		return job.next_run_at
			? `Next at ${formatDateTime(job.next_run_at)}`
			: 'The schedule cannot be read';
	}
</script>

<div class="pricing-tabs"><Tabs label="Pricing" tabs={PRICING_TABS} /></div>

<ObjectPage
	title="Price tracker"
	description="Checks the pages each provider is assigned every day, and scans the providers' sites for such pages every week. When a page changes, the prices and properties it states apply to the models; a pinned price is left alone."
	{data}
	{form}
>
	{#if view}
		{#if view.problems.length > 0}
			<Notice tone="warning" title="The price tracker cannot do all its work">
				<ul class="problems">
					{#each view.problems as problem (problem)}<li>{problem}</li>{/each}
				</ul>
			</Notice>
		{/if}
		<section class="card stack">
			<h2>State</h2>
			<dl class="facts">
				<dt>Tracker</dt>
				<dd>{view.enabled ? 'Switched on' : 'Switched off'}</dd>
				<dt>Scraper</dt>
				<dd>
					{#if view.scraper.present}
						Running{view.scraper.version ? `, version ${view.scraper.version}` : ''}{view.scraper
							.browser?.ok
							? `, browser ${view.scraper.browser.product}`
							: ''}
					{:else if view.scraper.last_seen_at}
						Not seen since {formatDateTime(view.scraper.last_seen_at)}
					{:else}
						Never seen: is gw-scraper running?
					{/if}
				</dd>
				<dt>Check of the pages</dt>
				<dd>
					{stateOf(view.jobs.check, view.enabled)}{view.jobs.check.last_started_at
						? `; last started ${formatDateTime(view.jobs.check.last_started_at)}`
						: ''}
				</dd>
				<dt>Scan for pages</dt>
				<dd>
					{stateOf(view.jobs.scan, view.enabled)}{view.jobs.scan.last_started_at
						? `; last started ${formatDateTime(view.jobs.scan.last_started_at)}`
						: ''}
				</dd>
				<dt>Providers read</dt>
				<dd>
					{view.tracked_providers.length === 0
						? 'None yet: confirm pages under Pricing pages'
						: view.tracked_providers.length}
				</dd>
			</dl>
			{#if writable}
				<div class="row">
					<form method="POST" action="?/run" use:enhance>
						<button
							type="submit"
							class="btn"
							disabled={view.jobs.check.running || view.jobs.check.run_requested}>Check now</button
						>
					</form>
					<form method="POST" action="?/scan" use:enhance>
						<button
							type="submit"
							class="btn"
							disabled={view.jobs.scan.running || view.jobs.scan.run_requested}>Scan now</button
						>
					</form>
					<a class="btn" href="/platform/price-tracker/pages">Pricing pages</a>
					<button type="button" class="btn" onclick={() => (importing = true)}>Import prices</button
					>
				</div>
			{/if}
		</section>
		<h2>Settings</h2>
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

{#if data.proposalsProblem}
	<section class="stack after"><ProblemNotice problem={data.proposalsProblem} /></section>
{:else if data.proposals.length > 0}
	<section class="stack after">
		<h2>Proposals of an earlier release</h2>
		<p class="muted small">
			The price tracker of earlier releases left these waiting. A price a check applies settles a
			model's proposal; approve or reject the others.
		</p>
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
	</section>
{/if}

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
			caption="Runs of the price tracker"
			rows={runs}
			key="id"
			fields={['started_at', 'job', 'trigger', 'result', 'outcomes', 'message']}
			labels={{ started_at: 'Started (UTC)' }}
			empty="The price tracker has not run yet."
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
			Each price becomes a new version of its model's price, in effect at once. A pinned price is
			left alone.
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

	.pricing-tabs {
		margin-bottom: var(--space-5);
	}

	.problems {
		margin: var(--space-1) 0 0;
		padding-left: var(--space-4);
	}
</style>
