<script lang="ts">
	import type { Schemas } from '#lib/api/types.ts';
	import ActionDialog from '#lib/components/ActionDialog.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import Meter from '#lib/components/Meter.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ResourcePage from '#lib/components/ResourcePage.svelte';
	import type { Column } from '#lib/components/table.ts';
	import { formatDateTime } from '#lib/format.ts';
	import { limitAmount, quantity } from '#lib/limits.ts';
	import { organisationPath } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	type State = Schemas['LimitState'];

	let { data, form, params }: PageProps = $props();

	const workspace = $derived(data.workspaces.find((ws) => ws.id === params.ws)?.label ?? params.ws);
	const organisation = $derived(
		data.orgs.find((org) => org.id === params.org)?.label ?? params.org
	);
	const subject = $derived(data.stateFor.end_user || data.stateFor.key_id);

	let resetting = $state<State | null>(null);
	let resetOpen = $state(false);

	const stateColumns: Column<State>[] = [
		{ key: 'name', label: 'Limit', kind: 'code' },
		{ key: 'subject', label: 'Counts', value: (s) => s.subject.replaceAll('_', ' ') },
		{ key: 'use', label: 'Used of the allowance' },
		{ key: 'remaining', label: 'Remaining', value: (s) => quantity(s.remaining, s.metric) },
		{
			key: 'resets_at',
			label: 'Resets (UTC)',
			value: (s) => (s.resets_at ? formatDateTime(s.resets_at) : 'Never')
		},
		{
			key: 'enforcement',
			label: 'When used up',
			value: (s) => (s.enforcement === 'request' ? 'Requests are rejected' : 'Reported only')
		}
	];
</script>

<ResourcePage
	title="Limits of {workspace}"
	description="What this workspace, each of its API keys or each of its end users may consume: tokens, requests or spend per window. Every limit that applies to a call must have room."
	crumbs={[{ label: organisation, href: organisationPath(params.org) }]}
	{data}
	{form}
	columns={[
		{ key: 'name', label: 'Name', kind: 'code' },
		{ key: 'amount', label: 'Allowance', value: limitAmount },
		{
			key: 'subject',
			label: 'Counts',
			value: (l) =>
				`each ${l.subject.replaceAll('_', ' ')}`.replace('each workspace', 'the workspace')
		},
		{
			key: 'enforcement',
			label: 'When used up',
			value: (l) => (l.enforcement === 'request' ? 'Requests are rejected' : 'Reported only')
		},
		{ key: 'alert_thresholds', label: 'Alerts at %', kind: 'list' },
		{ key: 'status', label: 'Status', kind: 'status' }
	]}
	empty="This workspace has no limits: only the caps of its organisation apply. Add a limit to cap its tokens, requests or spend."
	deleteNote="The limit no longer applies. Its counters and the end-user overrides that refer to it are discarded."
/>

<section class="stack">
	<h2>Current use</h2>
	<p class="muted">
		{#if subject}
			What <strong>{subject}</strong> has used of each limit that counts it.
		{:else}
			What the workspace has used. Limits that count each key or each end user show their state once
			you name one.
		{/if}
	</p>
	<form method="GET" class="filters">
		<div class="field">
			<label for="state-user">End user</label>
			<input id="state-user" name="end_user" value={data.stateFor.end_user} autocomplete="off" />
		</div>
		<div class="field">
			<label for="state-key">API key id</label>
			<input id="state-key" name="key_id" value={data.stateFor.key_id} autocomplete="off" />
		</div>
		<button type="submit" class="btn">Show the state</button>
	</form>

	{#if data.stateProblem}
		<ProblemNotice problem={data.stateProblem} />
	{:else if data.state.length === 0}
		<div class="card muted">
			{data.items.length === 0
				? 'There is no limit to report on.'
				: 'No limit has a state for this selection yet.'}
		</div>
	{:else}
		<DataTable
			caption="Current use of the limits"
			columns={stateColumns}
			rows={data.state}
			rowKey={(s) => s.limit_id}
			cell={use}
			custom={['use']}
			actions={data.canDelete ? reset : undefined}
		/>
	{/if}
</section>

{#snippet use(state: State)}
	<Meter
		used={state.used}
		amount={state.amount}
		label="{quantity(state.used, state.metric)} of {quantity(state.amount, state.metric)}"
	/>
{/snippet}

{#snippet reset(state: State)}
	<!-- Only the workspace's own limits can be reset here, not the caps of its organisation. -->
	{#if data.items.some((limit) => limit.id === state.limit_id)}
		<button
			type="button"
			class="btn btn-quiet danger"
			onclick={() => {
				resetting = state;
				resetOpen = true;
			}}
		>
			Reset<span class="visually-hidden"> {state.name}</span>
		</button>
	{/if}
{/snippet}

<ActionDialog
	bind:open={resetOpen}
	title="Reset limit “{resetting?.name ?? ''}”"
	action="?/reset"
	submit="Reset counters"
	outcome={form}
	fields={resetting && resetting.subject !== 'workspace' ? data.resetFields : []}
	object={{ subject }}
	mode="replace"
	hidden={{ id: resetting?.limit_id, name: resetting?.name }}
	danger
>
	<p>
		{#if resetting?.subject === 'workspace'}
			The counters of this limit start again from zero for the whole workspace.
		{:else}
			The counters of this limit start again from zero for one {resetting?.subject.replaceAll(
				'_',
				' '
			)}.
		{/if}
		What was used is not billed differently; only the limit forgets it.
	</p>
</ActionDialog>

<style>
	section {
		margin-top: var(--space-5);
	}

	.filters {
		display: flex;
		flex-wrap: wrap;
		align-items: flex-end;
		gap: var(--space-3);
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		min-width: 12rem;
	}
</style>
