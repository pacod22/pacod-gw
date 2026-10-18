<script lang="ts">
	import { enhance } from '$app/forms';
	import Search from '@lucide/svelte/icons/search';
	import ActionDialog from '#lib/components/ActionDialog.svelte';
	import ConfirmDialog from '#lib/components/ConfirmDialog.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import { filledColumns, type Column } from '#lib/components/table.ts';
	import {
		byProvider,
		draftNames,
		matchesQuery,
		nextRun,
		presetsByFamily,
		reportedOf,
		runResults,
		runState,
		takesOf,
		type ProviderGroup,
		type Reported
	} from '#lib/discovery.ts';
	import { formatDateTime, formatInteger } from '#lib/format.ts';
	import { MODEL_TABS } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	type Proposal = (typeof data.proposals)[number];
	/** A new model as a table row: the proposal with what the provider reported about it. */
	type Row = Proposal & Reported & { features: string; starts: string };

	const KINDS = ['chat', 'embedding', 'speech', 'decision'];

	const rows: Row[] = $derived(
		data.proposals
			.filter((p) => p.kind === 'new')
			.map((p) => {
				const reported = reportedOf(p.upstream_id, p.details);
				return {
					...p,
					// The core sends no name for a provider that was deleted since.
					provider: p.provider || p.provider_id,
					...reported,
					features: takesOf(reported),
					// A preset named after its kind (embedding, speech, decision) is said once.
					starts: [...new Set([p.suggested_kind, p.suggested_preset].filter(Boolean))].join(' · ')
				};
			})
	);
	const gone = $derived(
		data.proposals
			.filter((p) => p.kind === 'gone')
			.map((p) => ({ ...p, model: String((p.details as Record<string, unknown>).model ?? '') }))
	);

	let query = $state('');
	const groups = $derived(byProvider(rows.filter((r) => matchesQuery(r, query))));
	const filtering = $derived(query.trim() !== '');

	// The last run, and what it found at each provider.
	const last = $derived(data.runs[0]);
	const lastState = $derived(last ? runState(last, new Date()) : null);
	const running = $derived(lastState === 'running');
	const results = $derived(runResults(last?.result));
	const failures = $derived(results.filter((r) => r.error));
	const listed = $derived(new Map(results.map((r) => [r.provider, r.listed])));
	const next = $derived(nextRun(last?.started_at, data.intervalHours, new Date()));
	const TRIGGERS: Record<string, string> = { schedule: 'on schedule', manual: 'asked for' };

	const columns: Column<Row>[] = [
		{ key: 'upstream_id', label: 'Provider’s id', kind: 'code' },
		{ key: 'display_name', label: 'Name' },
		{ key: 'context_window', label: 'Context window', kind: 'number' },
		{ key: 'max_output_tokens', label: 'Output limit', kind: 'number' },
		{ key: 'features', label: 'Reported' },
		{ key: 'starts', label: 'Starts as' },
		{ key: 'first_seen', label: 'First seen (UTC)', kind: 'datetime' }
	];
	/** What only some providers report: a group whose provider reports none of it hides it. */
	const OPTIONAL = ['display_name', 'context_window', 'max_output_tokens', 'features'];

	// Adding a model: one dialog, filled from the row whose button was pressed. The choice
	// between the two usual names stays for the next model; everything else starts from the
	// row's suggestions.
	let adding = $state<Row | null>(null);
	let addOpen = $state(false);
	let naming = $state<'prefixed' | 'bare' | 'custom'>('prefixed');
	// Kept apart from `naming`: a successful submission resets the form, and Svelte copies the
	// reset controls back into their bound state. A reset fires no change event.
	let bare = $state(false);
	let customName = $state('');
	let kind = $state('chat');
	let preset = $state('');
	const names = $derived(adding ? draftNames(adding.provider, adding.upstream_id) : null);
	const presetGroups = $derived(presetsByFamily(data.presets));
	const presetHelp = $derived(data.presets.find((p) => p.name === preset)?.description ?? '');

	function openAdd(row: Row) {
		adding = row;
		kind = row.suggested_kind ?? 'chat';
		preset = row.suggested_preset ?? '';
		customName = '';
		naming = bare ? 'bare' : 'prefixed';
		addOpen = true;
	}

	// Dismissing every model of a provider shown, after asking.
	let dismissing = $state<ProviderGroup<Row> | null>(null);
	let dismissOpen = $state(false);

	function openDismissAll(group: ProviderGroup<Row>) {
		dismissing = group;
		dismissOpen = true;
	}

	const accepted = $derived(
		form?.ok && form.action === 'accept'
			? (form.data as { model_id: string; name: string } | undefined)
			: undefined
	);
</script>

<div class="stack-lg">
	<PageHeader
		title="Model discovery"
		description="Compares the model list of every provider with the repository, on a schedule or when you ask. Nothing is created until you add a model as a draft."
	>
		{#snippet actions()}
			<form method="POST" action="?/run" use:enhance>
				<button type="submit" class="btn btn-primary" disabled={running}>
					{running ? 'Running…' : 'Run now'}
				</button>
			</form>
		{/snippet}
	</PageHeader>
	<Tabs label="Models" tabs={MODEL_TABS} />

	{#if accepted}
		<Notice tone="success">
			<p>
				Draft model <a href="/platform/models/{accepted.model_id}" class="mono">{accepted.name}</a>
				created. Open it to give it a price or mark it free, check its capabilities, then activate it.
			</p>
		</Notice>
	{:else}
		<OutcomeNotice outcome={form} always={['run', 'dismiss']} />
	{/if}
	{#if data.problem}<ProblemNotice problem={data.problem} />{/if}

	<section class="card" aria-label="State of discovery">
		<dl class="facts">
			<dt>Last run (UTC)</dt>
			<dd>
				{#if !last}
					Never
				{:else}
					{formatDateTime(last.started_at)}, {TRIGGERS[last.trigger] ?? last.trigger}:
					{#if lastState === 'running'}
						running
					{:else if lastState === 'unfinished'}
						did not finish
					{:else if failures.length > 0}
						{failures.length} of {results.length} providers could not be asked
					{:else if lastState === 'failed'}
						failed{last.message ? `: ${last.message}` : ''}
					{:else}
						finished, {results.length}
						{results.length === 1 ? 'provider' : 'providers'} asked
					{/if}
				{/if}
			</dd>
			<dt>Schedule</dt>
			<dd>
				{#if data.intervalHours === null}
					Not known: the platform settings could not be read.
				{:else if data.intervalHours === 0}
					Only when asked.
				{:else}
					Every {data.intervalHours === 1 ? 'hour' : `${data.intervalHours} hours`}; next run
					{next === 'due' ? 'due now' : formatDateTime(next)}.
				{/if}
				<a href="/platform/settings">Platform settings</a>
			</dd>
			<dt>Waiting for a decision</dt>
			<dd>
				{rows.length}
				{rows.length === 1 ? 'new model' : 'new models'}, {gone.length} gone
			</dd>
		</dl>
	</section>

	{#if failures.length > 0}
		<Notice tone="warning" title="Not every provider could be asked in the last run">
			<ul class="failures">
				{#each failures as failure (failure.provider)}
					<li><strong>{failure.provider}</strong>: {failure.error}</li>
				{/each}
			</ul>
		</Notice>
	{/if}

	<section class="stack" aria-labelledby="new-heading">
		<div class="section-head">
			<h2 id="new-heading">New at the providers <span class="count">{rows.length}</span></h2>
			{#if rows.length > 8 || filtering}
				<label class="search">
					<Search size={16} aria-hidden="true" />
					<span class="visually-hidden">Filter the new models</span>
					<input
						type="search"
						placeholder="Filter by id, name or provider"
						bind:value={query}
						autocomplete="off"
					/>
				</label>
			{/if}
		</div>
		{#if rows.length === 0}
			<div class="card muted">Nothing new: the repository has every model the providers list.</div>
		{:else if groups.length === 0}
			<div class="card muted">No new model matches “{query.trim()}”.</div>
		{:else}
			{#each groups as group (group.provider_id)}
				{@const count = group.rows.length}
				{@const total = listed.get(group.provider)}
				{@const summary =
					`${count} new${filtering ? ' shown' : ''}` +
					(typeof total === 'number' ? ` of ${formatInteger(total)} listed` : '')}
				<section class="provider" aria-labelledby="group-{group.provider_id}">
					<div class="group-head">
						<h3 id="group-{group.provider_id}">
							<a
								href="/platform/providers/{group.provider_id}/discovery"
								title="Every model {group.provider} lists">{group.provider}</a
							>
						</h3>
						<span class="muted small">{summary}</span>
						{#if count > 1}
							<button type="button" class="btn btn-quiet" onclick={() => openDismissAll(group)}>
								{filtering ? `Dismiss these ${count}` : `Dismiss all ${count}`}<span
									class="visually-hidden"
								>
									of {group.provider}</span
								>
							</button>
						{/if}
					</div>
					<DataTable
						caption="New models at {group.provider}"
						rows={group.rows}
						rowKey={(r) => r.id}
						columns={filledColumns(columns, group.rows, OPTIONAL)}
						actions={newActions}
					/>
				</section>
			{/each}
		{/if}
	</section>

	<section class="stack" aria-labelledby="gone-heading">
		<h2 id="gone-heading">Gone from the providers <span class="count">{gone.length}</span></h2>
		{#if gone.length === 0}
			<div class="card muted">Every model of the repository is still listed by its provider.</div>
		{:else}
			<p class="muted small">
				Nothing is retired by itself: open the model to deprecate or retire it, or dismiss the flag
				if the provider still serves it.
			</p>
			<DataTable
				caption="Repository models the providers no longer list"
				rows={gone}
				rowKey={(p) => p.id}
				columns={[
					{
						key: 'model',
						label: 'Model',
						kind: 'code',
						href: (p) => (p.model_id ? `/platform/models/${p.model_id}` : null)
					},
					{ key: 'provider', label: 'Provider' },
					{ key: 'upstream_id', label: 'Provider’s id', kind: 'code' },
					{ key: 'first_seen', label: 'Missing since (UTC)', kind: 'datetime' }
				]}
				actions={dismissOne}
			/>
		{/if}
	</section>
</div>

{#snippet newActions(row: Row)}
	<button type="button" class="btn btn-quiet" onclick={() => openAdd(row)}>
		Add as draft…<span class="visually-hidden"> {row.upstream_id}</span>
	</button>
	{@render dismissOne(row)}
{/snippet}

{#snippet dismissOne(p: Proposal)}
	<form method="POST" action="?/dismiss" use:enhance class="inline">
		<input type="hidden" name="id" value={p.id} />
		<button type="submit" class="btn btn-quiet">
			Dismiss<span class="visually-hidden"> {p.upstream_id}</span>
		</button>
	</form>
{/snippet}

<ActionDialog
	bind:open={addOpen}
	title={adding ? `Add ${adding.upstream_id} as a draft` : 'Add as a draft'}
	action="?/accept"
	submit="Add as draft"
	outcome={form}
	hidden={{ id: adding?.id, suggested_preset: adding?.suggested_preset ?? undefined }}
	ready={naming !== 'custom' || customName.trim() !== ''}
>
	{#if adding && names}
		<dl class="facts">
			<dt>Provider</dt>
			<dd>{adding.provider}</dd>
			{#if adding.display_name}<dt>Name</dt>
				<dd>{adding.display_name}</dd>{/if}
			{#if adding.context_window !== null}<dt>Context window</dt>
				<dd>{formatInteger(adding.context_window)} tokens</dd>{/if}
			{#if adding.max_output_tokens !== null}<dt>Output limit</dt>
				<dd>{formatInteger(adding.max_output_tokens)} tokens</dd>{/if}
			{#if adding.features}<dt>Reported</dt>
				<dd>{adding.features}</dd>{/if}
		</dl>
		{#if !adding.context_window && !adding.max_output_tokens && !adding.features}
			<p class="help">
				The provider reports nothing more about this model: fill in its limits on the draft.
			</p>
		{/if}

		<fieldset class="naming">
			<legend>Name in the repository</legend>
			<label class="check">
				<input
					type="radio"
					name="naming"
					value="prefixed"
					bind:group={naming}
					onchange={() => (bare = false)}
				/>
				<span><code>{names.prefixed}</code></span>
			</label>
			<label class="check">
				<input
					type="radio"
					name="naming"
					value="bare"
					bind:group={naming}
					onchange={() => (bare = true)}
				/>
				<span
					><code>{names.bare}</code><span class="muted">
						– the provider’s id alone, as some applications expect</span
					></span
				>
			</label>
			<label class="check">
				<input type="radio" name="naming" value="custom" bind:group={naming} />
				<span>Another name</span>
			</label>
			{#if naming === 'custom'}
				<label class="visually-hidden" for="add-name">Another name</label>
				<input
					id="add-name"
					name="name"
					class="mono"
					bind:value={customName}
					placeholder={names.prefixed}
					autocomplete="off"
					spellcheck="false"
					required
				/>
			{/if}
		</fieldset>

		<div class="pair">
			<div class="field">
				<label for="add-kind">Kind</label>
				<select id="add-kind" name="kind" bind:value={kind}>
					{#each KINDS as k (k)}
						<option value={k}>{k}{k === adding.suggested_kind ? ' (suggested)' : ''}</option>
					{/each}
				</select>
			</div>
			<div class="field">
				<label for="add-preset">Capability preset</label>
				<select id="add-preset" name="capability_preset" bind:value={preset}>
					{#each presetGroups as group (group.family)}
						<optgroup label={group.family}>
							{#each group.presets as p (p.name)}
								<option value={p.name}
									>{p.name}{p.name === adding.suggested_preset ? ' (suggested)' : ''}</option
								>
							{/each}
						</optgroup>
					{/each}
				</select>
			</div>
		</div>
		{#if presetHelp}<p class="help">{presetHelp}</p>{/if}
		<p class="help">
			The draft is not callable yet: give it a price or mark it free, check its capabilities, then
			activate it.
		</p>
	{/if}
</ActionDialog>

{#if dismissing}
	<ConfirmDialog
		bind:open={dismissOpen}
		title="Dismiss {dismissing.rows.length} models of {dismissing.provider}"
		action="?/dismiss"
		confirm="Dismiss {dismissing.rows.length}"
		outcome={form}
		hidden={{ ids: dismissing.rows.map((r) => r.id).join(' ') }}
	>
		<p>
			A later run does not propose them again. The provider’s own page, under Providers, still lists
			every model it offers and can create a draft from any of them.
		</p>
	</ConfirmDialog>
{/if}

<style>
	.count {
		margin-left: var(--space-1);
		color: var(--text-muted);
		font-weight: 500;
	}

	.section-head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-2) var(--space-4);
	}

	.search {
		position: relative;
		display: flex;
		align-items: center;
		flex: 0 1 20rem;
		min-width: 12rem;
		color: var(--text-muted);
	}

	.search :global(svg) {
		position: absolute;
		left: 0.75rem;
		pointer-events: none;
	}

	.search input {
		padding-left: 2.25rem;
		background: var(--surface);
	}

	.provider {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.provider + .provider {
		margin-top: var(--space-3);
	}

	.group-head {
		display: flex;
		flex-wrap: wrap;
		align-items: baseline;
		gap: var(--space-1) var(--space-3);
	}

	.group-head h3 {
		font-size: var(--text-base, 1rem);
	}

	.group-head .btn {
		margin-left: auto;
	}

	.failures {
		margin: 0;
		padding-left: 1.25rem;
	}

	.pair {
		display: grid;
		grid-template-columns: minmax(0, 1fr) minmax(0, 2fr);
		gap: var(--space-3);
	}

	@media (max-width: 30rem) {
		.pair {
			grid-template-columns: 1fr;
		}
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		min-width: 0;
	}

	.naming {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
	}

	.check {
		display: flex;
		align-items: baseline;
		gap: var(--space-2);
		font-weight: 500;
		overflow-wrap: anywhere;
	}

	.check input {
		flex: none;
		position: relative;
		top: 0.15rem;
	}

	.help {
		color: var(--text-muted);
		font-size: var(--text-sm);
	}
</style>
