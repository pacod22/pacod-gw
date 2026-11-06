<script lang="ts">
	import { enhance } from '$app/forms';
	import ActionDialog from '#lib/components/ActionDialog.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import ObjectPage from '#lib/components/ObjectPage.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import { formatDateTime } from '#lib/format.ts';
	import { PRICING_TABS } from '#lib/nav.ts';
	import { runSummary } from '#lib/prices.ts';
	import type { Row } from '#lib/report/format.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const view = $derived(data.item);
	const writable = $derived(data.fields !== null);
	let uploading = $state(false);
	const uploaded = $derived(
		form?.ok && form.action === 'upload' && Array.isArray(form.data) ? (form.data as Row[]) : null
	);

	const SOURCES: Record<string, string> = {
		pacod_aggregator: 'pacod-aggregator',
		openrouter: 'OpenRouter',
		litellm: 'LiteLLM',
		file: 'The models file'
	};
	const JOBS: Record<string, string> = {
		catalog_sync: 'Sync',
		catalog_file: 'Models file',
		catalog_upload: 'Upload'
	};
	// The sync's own problems; the price tracker's are on its page.
	const problems = $derived((view?.problems ?? []).filter((p) => p.startsWith('The model data')));
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
	const fileRun = $derived(view?.sync.models_file?.last_read ?? null);
</script>

<div class="pricing-tabs"><Tabs label="Pricing" tabs={PRICING_TABS} /></div>

<ObjectPage
	title="Model data"
	description="Model information and prices from outside the gateway: synced every day from pacod-aggregator, OpenRouter or LiteLLM, read from the models file of the configuration first, or taken from a file you upload. Every valid price applies unless it is pinned; models the source lists that the repository lacks are proposed under Models."
	{data}
	{form}
>
	{#if view}
		{#if problems.length > 0}
			<Notice tone="warning" title="The sync cannot do its work">
				<ul class="problems">
					{#each problems as problem (problem)}<li>{problem}</li>{/each}
				</ul>
			</Notice>
		{/if}
		<section class="card stack">
			<h2>State</h2>
			<dl class="facts">
				<dt>Source</dt>
				<dd>{view.source ? SOURCES[view.source] : 'None: no sync'}</dd>
				<dt>Sync</dt>
				<dd>
					{#if view.sync.running}
						Running now
					{:else if view.sync.run_requested}
						Asked for: it starts within a minute
					{:else if !view.enabled || !view.source}
						Off
					{:else if view.sync.next_run_at}
						Next at {formatDateTime(view.sync.next_run_at)}
					{:else}
						The schedule cannot be read
					{/if}{view.sync.last_started_at
						? `; last started ${formatDateTime(view.sync.last_started_at)}`
						: ''}
				</dd>
				<dt>pacod-aggregator</dt>
				<dd>
					{#if view.sync.aggregator.configured}
						<span class="mono">{view.sync.aggregator.url}</span>{view.sync.aggregator.token
							? ', with a token'
							: ', without a token'}
					{:else}
						Not set up (PGW_AGGREGATOR_URL)
					{/if}
				</dd>
				<dt>Models file</dt>
				<dd>
					{#if view.sync.models_file}
						<span class="mono">{view.sync.models_file.path}</span>{fileRun
							? `; read ${formatDateTime(fileRun.started_at)}, ${fileRun.ok ? runSummary(fileRun.result) || 'nothing to apply' : `failed: ${fileRun.message}`}`
							: '; not read yet'}
					{:else}
						None (PGW_MODELS_FILE)
					{/if}
				</dd>
			</dl>
			{#if view.sync.seeded.length > 0}
				<p class="muted small">
					Set by the environment at every start: {view.sync.seeded.join(', ')}. Saving here takes
					them over, and the environment no longer changes them.
				</p>
			{/if}
			{#if writable}
				<div class="row">
					<form method="POST" action="?/run" use:enhance>
						<button
							type="submit"
							class="btn"
							disabled={!view.source || view.sync.running || view.sync.run_requested}
							>Update now</button
						>
					</form>
					<button type="button" class="btn" onclick={() => (uploading = true)}>Upload a file</button
					>
				</div>
			{/if}
		</section>
		<h2>Settings</h2>
	{/if}
</ObjectPage>

{#if uploaded}
	<section class="stack after">
		<h2>Result of the upload</h2>
		<ReportTable
			caption="Result of the upload"
			rows={uploaded}
			key="model"
			empty="The file named no model of the repository."
		/>
	</section>
{/if}

<section class="stack after">
	<h2>Runs</h2>
	{#if data.runsProblem}
		<ProblemNotice problem={data.runsProblem} />
	{:else}
		<ReportTable
			caption="Runs of the model data"
			rows={runs}
			key="id"
			fields={['started_at', 'job', 'trigger', 'result', 'outcomes', 'message']}
			labels={{ started_at: 'Started (UTC)' }}
			empty="No model data has been read yet."
		/>
	{/if}
</section>

<ActionDialog
	bind:open={uploading}
	title="Upload a file"
	action="?/upload"
	submit="Apply file"
	outcome={form}
	enctype="multipart/form-data"
>
	<p class="muted small">
		A catalogue (<span class="mono">pacod.catalog/v1</span>, as pacod-aggregator writes it),
		OpenRouter's or LiteLLM's list of models, or a pacod price file, of at most 16 MB. Its prices
		apply at once, unless pinned, and so do the properties it states where they differ from the
		model's.
	</p>
	<label for="upload-file">File</label>
	<input id="upload-file" type="file" name="file" accept="application/json,.json" required />
	<label for="upload-format">Format</label>
	<select id="upload-format" name="format">
		<option value="auto">Found from the file</option>
		<option value="catalog">Catalogue (pacod.catalog/v1)</option>
		<option value="openrouter">OpenRouter</option>
		<option value="litellm">LiteLLM</option>
		<option value="pacod">pacod price file</option>
	</select>
</ActionDialog>

<style>
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
