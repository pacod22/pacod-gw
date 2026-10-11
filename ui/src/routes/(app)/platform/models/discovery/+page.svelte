<script lang="ts">
	import { enhance } from '$app/forms';
	import DataTable from '#lib/components/DataTable.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import { formatDateTime } from '#lib/format.ts';
	import { MODEL_TABS } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	type Proposal = (typeof data.proposals)[number];
	const last = $derived(data.runs[0]);
	const added = $derived(data.proposals.filter((p) => p.kind === 'new'));
	const gone = $derived(data.proposals.filter((p) => p.kind === 'gone'));
	const detail = (p: Proposal, key: string) => {
		const v = (p.details as Record<string, unknown>)[key];
		return typeof v === 'number' || typeof v === 'string' ? String(v) : '';
	};
	/** What the provider said the model can do, where it says. */
	const reported = (p: Proposal) => {
		const d = p.details as Record<string, unknown>;
		return [d.vision === true && 'images', d.reasons === true && 'reasoning']
			.filter(Boolean)
			.join(', ');
	};
	let naming: 'prefixed' | 'bare' = $state('prefixed');
</script>

<div class="stack-lg">
	<PageHeader
		title="Model discovery"
		description="Every provider is asked for its model list, on the schedule of the platform settings or when you ask. A model the provider lists and the repository lacks is proposed as a draft; a model the provider no longer lists is flagged. Nothing is created or activated until you accept."
	>
		{#snippet actions()}
			<form method="POST" action="?/run" use:enhance>
				<button type="submit" class="btn btn-primary">Run now</button>
			</form>
		{/snippet}
	</PageHeader>
	<Tabs label="Models" tabs={MODEL_TABS} />
	<OutcomeNotice outcome={form} />
	{#if data.problem}<ProblemNotice problem={data.problem} />{/if}

	<p class="muted">
		{#if last}
			Last run {formatDateTime(last.started_at)} ({last.trigger}){#if last.finished_at},
				{last.ok ? 'finished' : `finished with problems: ${last.message}`}{:else}, still running{/if}.
		{:else}
			Discovery has not run yet.
		{/if}
	</p>

	<section class="stack">
		<h2>New at the providers</h2>
		{#if added.length === 0}
			<p class="muted">Nothing new: the repository has every model the providers list.</p>
		{:else}
			<div class="field naming">
				<label for="naming">Name an added model</label>
				<select id="naming" bind:value={naming}>
					<option value="prefixed"
						>provider/the provider’s id, for example gemini/gemini-3.8-flash</option
					>
					<option value="bare">the provider’s id alone, for example gemini-3.8-flash</option>
				</select>
				<p class="muted small">
					Each model starts from the kind and capability preset its id and the provider's report
					suggest; change them per row before adding it.
				</p>
			</div>
			<DataTable
				caption="Models the providers list and the repository lacks"
				rows={added}
				rowKey={(p) => p.id}
				columns={[
					{ key: 'provider', label: 'Provider' },
					{ key: 'upstream_id', label: 'The provider’s id', kind: 'code' },
					{ key: 'display_name', label: 'Name', value: (p) => detail(p, 'display_name') },
					{ key: 'context_window', label: 'Context', value: (p) => detail(p, 'context_window') },
					{ key: 'reported', label: 'Takes', value: reported },
					{ key: 'first_seen', label: 'First seen (UTC)', kind: 'datetime' }
				]}
				actions={newActions}
			/>
		{/if}
	</section>

	<section class="stack">
		<h2>Gone from the providers</h2>
		{#if gone.length === 0}
			<p class="muted">Every model of the repository is still listed by its provider.</p>
		{:else}
			<DataTable
				caption="Repository models the providers no longer list"
				rows={gone}
				rowKey={(p) => p.id}
				columns={[
					{ key: 'provider', label: 'Provider' },
					{ key: 'model', label: 'Model', kind: 'code', value: (p) => detail(p, 'model') },
					{ key: 'upstream_id', label: 'The provider’s id', kind: 'code' },
					{ key: 'first_seen', label: 'Missing since (UTC)', kind: 'datetime' }
				]}
				actions={dismissOnly}
			/>
		{/if}
	</section>
</div>

{#snippet newActions(p: Proposal)}
	<div class="accept">
		<form id="accept-{p.id}" method="POST" action="?/accept" use:enhance class="choices">
			<input type="hidden" name="id" value={p.id} />
			<input type="hidden" name="naming" value={naming} />
			<label class="visually-hidden" for="kind-{p.id}">Kind of {p.upstream_id}</label>
			<select id="kind-{p.id}" name="kind" value={p.suggested_kind ?? 'chat'}>
				<option value="chat">chat</option>
				<option value="embedding">embedding</option>
				<option value="speech">speech</option>
			</select>
			<label class="visually-hidden" for="preset-{p.id}">Capability preset of {p.upstream_id}</label
			>
			<select
				id="preset-{p.id}"
				name="capability_preset"
				value={p.suggested_preset ?? ''}
				title={data.presets.find((x) => x.name === p.suggested_preset)?.description ?? ''}
			>
				{#each data.presets as preset (preset.name)}
					<option value={preset.name}>{preset.name}</option>
				{/each}
			</select>
		</form>
		<div class="buttons">
			<button type="submit" form="accept-{p.id}" class="btn btn-quiet">
				Add as draft<span class="visually-hidden"> {p.upstream_id}</span>
			</button>
			{@render dismissOnly(p)}
		</div>
	</div>
{/snippet}

{#snippet dismissOnly(p: Proposal)}
	<form method="POST" action="?/dismiss" use:enhance class="inline">
		<input type="hidden" name="id" value={p.id} />
		<button type="submit" class="btn btn-quiet">
			Dismiss<span class="visually-hidden"> {p.upstream_id}</span>
		</button>
	</form>
{/snippet}

<style>
	.naming {
		max-width: 36rem;
	}
	.accept {
		display: flex;
		flex-direction: column;
		align-items: flex-end;
		gap: var(--space-1);
	}
	.choices,
	.buttons {
		display: flex;
		gap: var(--space-1);
	}
	.choices select {
		width: auto;
		max-width: 10rem;
		font-size: var(--text-sm, 0.875rem);
		padding-block: 0.1rem;
	}
</style>
