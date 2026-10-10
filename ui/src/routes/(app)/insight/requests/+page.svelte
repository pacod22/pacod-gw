<script lang="ts">
	import DataTable from '#lib/components/DataTable.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import Pager from '#lib/components/Pager.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import StageBar from '#lib/components/StageBar.svelte';
	import type { Column } from '#lib/components/table.ts';
	import { formatCost, formatDateTime, formatInteger, formatMs } from '#lib/format.ts';
	import { timingOf } from '#lib/insight/timing.ts';
	import { STATUSES, type UsageEvent } from '#lib/insight/usage.ts';
	import { scopeQuery } from '#lib/nav.ts';
	import { toDateTimeInput } from '#lib/schema/form.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const filters = $derived(data.filters);
	const scopeText = $derived.by(() => {
		const ws = data.workspaces.find((w) => w.id === data.scope.ws);
		const org = data.orgs.find((o) => o.id === data.scope.org);
		if (data.scope.ws) return `Requests of the workspace ${ws?.label ?? data.scope.ws}.`;
		if (data.scope.org) return `Requests of every workspace of ${org?.label ?? data.scope.org}.`;
		return 'Requests of every organisation and workspace you may see. Choose one in the bar at the top to narrow the log.';
	});
	const filtered = $derived(
		Object.entries(filters).some(
			([name, value]) => value !== '' && name !== 'organisation_id' && name !== 'workspace_id'
		) || Object.keys(data.tags).length > 0
	);

	/** A time filter may be in the URL as typed in the form or as RFC 3339. */
	const timeValue = (value: string) =>
		/^\d{4}-\d\d-\d\dT\d\d:\d\d$/.test(value) ? value : toDateTimeInput(value);

	const columns: Column<UsageEvent>[] = [
		{ key: 'timestamp', label: 'Time (UTC)' },
		{ key: 'status', label: 'Result' },
		{
			key: 'call',
			label: 'Call',
			value: (e) => `${e.model_served || e.model_requested} ${e.provider} ${e.mode}`,
			sortValue: (e) => e.model_served || e.model_requested
		},
		{ key: 'workspace_id', label: 'Workspace', kind: 'code' },
		{
			key: 'tokens',
			label: 'Tokens in / out, cost',
			value: (e) =>
				`${formatInteger(e.input_tokens)} / ${formatInteger(e.output_tokens)} ${formatCost(e.cost, e.currency)}`,
			sortValue: (e) => e.cost ?? null
		},
		{
			key: 'duration',
			label: 'Duration and stages',
			value: (e) => formatMs(e.total_ms),
			sortValue: (e) => e.total_ms ?? null
		}
	];
</script>

<div class="stack-lg">
	<PageHeader
		title="Requests"
		description="The request log, newest first: every call through the gateway with its result, its usage and where its time went."
	/>

	<p class="muted">{scopeText}</p>

	<form method="GET" class="card filters">
		{#if filters.organisation_id}
			<input type="hidden" name="organisation_id" value={filters.organisation_id} />
		{/if}
		{#if filters.workspace_id}
			<input type="hidden" name="workspace_id" value={filters.workspace_id} />
		{/if}
		<div class="field">
			<label for="f-from">From (UTC)</label>
			<input id="f-from" name="from" type="datetime-local" value={timeValue(filters.from ?? '')} />
		</div>
		<div class="field">
			<label for="f-to">To (UTC)</label>
			<input id="f-to" name="to" type="datetime-local" value={timeValue(filters.to ?? '')} />
		</div>
		<div class="field">
			<label for="f-status">Result</label>
			<select id="f-status" name="status" value={filters.status ?? ''}>
				<option value="">Any</option>
				{#each STATUSES as status (status)}
					<option value={status}>{status}</option>
				{/each}
			</select>
		</div>
		<div class="field">
			<label for="f-provider">Provider</label>
			<input id="f-provider" name="provider" value={filters.provider ?? ''} autocomplete="off" />
		</div>
		<div class="field">
			<label for="f-model">Model served</label>
			<input id="f-model" name="model" value={filters.model ?? ''} autocomplete="off" />
		</div>
		<div class="field">
			<label for="f-key">API key id</label>
			<input id="f-key" name="key_id" value={filters.key_id ?? ''} autocomplete="off" />
		</div>
		<div class="field">
			<label for="f-user">End user</label>
			<input id="f-user" name="end_user" value={filters.end_user ?? ''} autocomplete="off" />
		</div>
		<div class="field">
			<label for="f-agent">Agent</label>
			<input id="f-agent" name="agent_id" value={filters.agent_id ?? ''} autocomplete="off" />
		</div>
		<div class="field">
			<label for="f-task">Task</label>
			<input id="f-task" name="task" value={filters.task ?? ''} autocomplete="off" />
		</div>
		<div class="field">
			<label for="f-trace">Trace id</label>
			<input id="f-trace" name="trace_id" value={filters.trace_id ?? ''} autocomplete="off" />
		</div>
		<div class="field">
			<label for="f-session">Session id</label>
			<input id="f-session" name="session_id" value={filters.session_id ?? ''} autocomplete="off" />
		</div>
		{#if data.tagKeys === null}
			<div class="field">
				<label for="f-tag-key">Tag key</label>
				<input id="f-tag-key" name="tag_key" value={data.tags.tag_key ?? ''} autocomplete="off" />
			</div>
			<div class="field">
				<label for="f-tag-value">Tag value</label>
				<input
					id="f-tag-value"
					name="tag_value"
					value={data.tags.tag_value ?? ''}
					autocomplete="off"
				/>
			</div>
		{:else}
			{#each data.tagKeys as key (key)}
				<div class="field">
					<label for="f-tag-{key}">Tag {key}</label>
					<input
						id="f-tag-{key}"
						name="tag.{key}"
						value={data.tags[`tag.${key}`] ?? ''}
						autocomplete="off"
					/>
				</div>
			{/each}
		{/if}
		<div class="buttons">
			<button type="submit" class="btn btn-primary">Apply filters</button>
			<a class="btn" href="/insight/requests/export{data.search}" download data-sveltekit-reload>
				Download as CSV
			</a>
			{#if filters.trace_id || filters.session_id}
				<!-- One trace or session, every call of it, also outside the time range. -->
				<a
					class="btn"
					href="/insight/requests/jsonl?{new URLSearchParams(
						filters.trace_id
							? { trace_id: filters.trace_id }
							: { session_id: filters.session_id ?? '' }
					)}&audit=true"
					download
					data-sveltekit-reload
				>
					Export the {filters.trace_id ? 'trace' : 'session'} as JSON lines
				</a>
			{/if}
			{#if filtered}
				<a class="btn" href="/insight/requests{scopeQuery(data.scope)}">Clear filters</a>
			{/if}
		</div>
	</form>

	{#if data.problem}
		<ProblemNotice problem={data.problem} />
		{#if data.problem.status === 503}
			<p class="muted small">
				The request log is kept by the analytics service. Until it answers, calls are still served
				and recorded; they appear here once it is back.
			</p>
		{/if}
	{:else if data.requests.length === 0}
		<div class="card muted">
			{#if filtered || data.cursor}
				No request matches these filters{data.range.from
					? ` between ${formatDateTime(data.range.from)} and ${formatDateTime(data.range.to)} UTC`
					: ''}. Widen the time range or clear a filter.
			{:else}
				No requests were recorded in the last 24 hours. Create an API key in a workspace and call
				the gateway with it; the call appears here a few seconds later.
			{/if}
		</div>
	{:else}
		{#if data.range.from && data.range.to}
			<p class="muted small">
				{formatDateTime(data.range.from)} to {formatDateTime(data.range.to)} UTC
			</p>
		{/if}
		<DataTable
			caption="Requests"
			{columns}
			rows={data.requests}
			{cell}
			custom={['timestamp', 'status', 'call', 'tokens', 'duration']}
		/>
	{/if}
	<Pager cursor={data.cursor} nextCursor={data.nextCursor} count={data.requests.length} />
</div>

{#snippet cell(event: UsageEvent, column: Column<UsageEvent>)}
	{#if column.key === 'timestamp'}
		<a
			class="nowrap"
			href="/insight/requests/{encodeURIComponent(event.id)}{scopeQuery(data.scope)}"
		>
			{formatDateTime(event.timestamp, true)}
		</a>
	{:else if column.key === 'status'}
		<span class="result">
			<span class="dot {event.status}"></span>{event.status}
			<span class="muted">{event.http_status || ''}</span>
		</span>
	{:else if column.key === 'call'}
		<span class="call">
			<span>{event.model_served || event.model_requested}</span>
			<span class="muted small">{event.provider} · {event.mode}</span>
		</span>
	{:else if column.key === 'duration'}
		<span class="call">
			<span class="nowrap"
				>{formatMs(event.total_ms)}
				<span class="muted small">· gateway {formatMs(event.gateway_delay_ms)}</span></span
			>
			<StageBar timing={timingOf(event)} compact />
		</span>
	{:else if column.key === 'tokens'}
		<span class="call">
			<span class="nowrap"
				>{formatInteger(event.input_tokens)} / {formatInteger(event.output_tokens)}</span
			>
			<span class="muted small nowrap">{formatCost(event.cost, event.currency) || '–'}</span>
		</span>
	{/if}
{/snippet}

<style>
	.filters {
		display: grid;
		grid-template-columns: repeat(auto-fill, minmax(11rem, 1fr));
		gap: var(--space-3);
		align-items: end;
	}

	.field {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		min-width: 0;
	}

	.buttons {
		/* Below the fields, across the whole form. */
		grid-column: 1 / -1;
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2);
	}

	.nowrap {
		white-space: nowrap;
	}

	.call {
		display: flex;
		flex-direction: column;
		gap: 2px;
	}

	.result {
		display: inline-flex;
		align-items: center;
		gap: var(--space-2);
		white-space: nowrap;
	}

	/* The word carries the result; the dot only helps the eye down the column. */
	.dot {
		width: 0.5rem;
		height: 0.5rem;
		border-radius: 50%;
		background: var(--danger);
	}

	.dot.ok {
		background: var(--success);
	}

	.dot.cancelled {
		background: var(--border-strong);
	}

	.dot.timeout,
	.dot.rejected {
		background: var(--warning);
	}
</style>
