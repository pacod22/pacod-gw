<script lang="ts">
	import Badge from '#lib/components/Badge.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import StageBar from '#lib/components/StageBar.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import { canReadAuditRecords } from '#lib/access.ts';
	import { formatCost, formatDateTime, formatInteger, formatMs } from '#lib/format.ts';
	import { timingOf } from '#lib/insight/timing.ts';
	import { requestTabs, SCOPE_PARAMS } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, params }: PageProps = $props();

	const request = $derived(data.request);
	const timing = $derived(request ? timingOf(request) : null);
	const tags = $derived(request ? Object.entries(request.tags ?? {}) : []);
	const scope = $derived(
		request
			? `?organisation_id=${encodeURIComponent(request.organisation_id)}&workspace_id=${encodeURIComponent(request.workspace_id)}`
			: ''
	);
	// The audit record holds prompts and answers: the view is for those who may read them.
	const views = $derived(requestTabs(params.id, canReadAuditRecords(data.principal)));
</script>

<div class="stack-lg">
	<PageHeader
		title="Request {params.id}"
		crumbs={[{ label: 'Requests', href: `/insight/requests${scope}` }]}
	/>
	{#if views.length > 1}<Tabs
			label="Request"
			tabs={views}
			carry={SCOPE_PARAMS}
			variant="pills"
		/>{/if}

	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if request && timing}
		{#if request.status !== 'ok'}
			<Notice
				tone={request.status === 'error' ? 'error' : 'warning'}
				title="This call ended as “{request.status}”"
			>
				<p>
					{request.error_type
						? `${request.error_type.replaceAll('_', ' ')}: `
						: ''}{request.error_message || 'No message was recorded.'}
				</p>
			</Notice>
		{/if}

		<section class="card stack">
			<h2>Result</h2>
			<dl class="facts">
				<dt>Result</dt>
				<dd>
					<Badge value={request.status} /> <span class="muted">HTTP {request.http_status}</span>
				</dd>
				<dt>Time (UTC)</dt>
				<dd>{formatDateTime(request.timestamp, true)}</dd>
				<dt>Total time</dt>
				<dd>{formatMs(request.total_ms)}</dd>
				<dt>Gateway delay</dt>
				<dd>{formatMs(request.gateway_delay_ms)}</dd>
				<dt>Finish reason</dt>
				<dd>{request.finish_reason || '–'}</dd>
				<dt>Attempts</dt>
				<dd>{request.attempts}</dd>
				<dt>Streamed</dt>
				<dd>{request.stream ? 'Yes' : 'No'}</dd>
			</dl>
		</section>

		<section class="card stack">
			<h2>Where the time went</h2>
			<StageBar {timing} />
			<dl class="facts">
				{#if request.stream}
					<dt>Longest relay of a chunk</dt>
					<dd>{formatMs(request.relay_max_ms)}</dd>
				{/if}
				<dt>Accounting afterwards</dt>
				<dd>
					{formatMs(request.settle_ms)} <span class="muted">– the client does not wait for it</span>
				</dd>
			</dl>
		</section>

		<div class="columns">
			<section class="card stack">
				<h2>Routing</h2>
				<dl class="facts">
					<dt>Mode</dt>
					<dd>{request.mode}</dd>
					<dt>Interface</dt>
					<dd>{request.interface}</dd>
					<dt>Path</dt>
					<dd class="mono">{request.path || '–'}</dd>
					<dt>Provider</dt>
					<dd>{request.provider || '–'}</dd>
					<dt>Model requested</dt>
					<dd class="mono">{request.model_requested || '–'}</dd>
					<dt>Model served</dt>
					<dd class="mono">{request.model_served || '–'}</dd>
					{#if request.model_reported}
						<dt>Model the provider reported</dt>
						<dd class="mono">{request.model_reported}</dd>
					{/if}
					<dt>Template</dt>
					<dd>
						{#if request.template}
							{request.template} <span class="muted">version {request.template_version}</span>
						{:else}–{/if}
					</dd>
					<dt>Provider's request id</dt>
					<dd class="mono">{request.upstream_request_id || '–'}</dd>
					<dt>Dropped parameters</dt>
					<dd>{request.dropped_params?.length ? request.dropped_params.join(', ') : 'None'}</dd>
				</dl>
			</section>

			<section class="card stack">
				<h2>Who called</h2>
				<dl class="facts">
					<dt>Organisation</dt>
					<dd class="mono">{request.organisation_id}</dd>
					<dt>Workspace</dt>
					<dd class="mono">{request.workspace_id}</dd>
					<dt>API key</dt>
					<dd class="mono">
						<a href="/insight/requests{scope}&key_id={encodeURIComponent(request.key_id)}"
							>{request.key_id}</a
						>
					</dd>
					<dt>End user</dt>
					<dd>{request.end_user || '–'}</dd>
					<dt>Session</dt>
					<dd class="mono">
						{#if request.session_id}
							<a href="/insight/requests{scope}&session_id={encodeURIComponent(request.session_id)}"
								>{request.session_id}</a
							>
						{:else}–{/if}
					</dd>
					<dt>Trace</dt>
					<dd class="mono">
						{#if request.trace_id}
							<a href="/insight/traces/{encodeURIComponent(request.trace_id)}{scope}"
								>{request.trace_id}</a
							>
							<a
								class="small"
								href="/insight/requests{scope}&trace_id={encodeURIComponent(request.trace_id)}"
								>its calls in the log</a
							>
						{:else}–{/if}
					</dd>
					<dt>Tags</dt>
					<dd>
						{#if tags.length > 0}
							{#each tags as [key, value] (key)}<span class="tag">{key}: {value}</span>{/each}
						{:else}None{/if}
					</dd>
					<dt>Agent</dt>
					<dd class="mono">
						{#if request.agent_id}
							<a href="/insight/requests{scope}&agent_id={encodeURIComponent(request.agent_id)}"
								>{request.agent_id}</a
							>
						{:else}–{/if}
					</dd>
					<dt>User</dt>
					<dd>{request.user_id || '–'}</dd>
					<dt>Task</dt>
					<dd>{request.task || '–'}</dd>
					<dt>Complexity</dt>
					<dd>{request.complexity || '–'}</dd>
					<dt>Core instance</dt>
					<dd class="mono">{request.instance_id}</dd>
				</dl>
			</section>

			<section class="card stack">
				<h2>Usage and cost</h2>
				<dl class="facts">
					<dt>Input tokens</dt>
					<dd>{formatInteger(request.input_tokens)}</dd>
					<dt>Read from cache</dt>
					<dd>{formatInteger(request.cache_read_tokens)}</dd>
					<dt>Written to cache</dt>
					<dd>{formatInteger(request.cache_write_tokens)}</dd>
					<dt>Output tokens</dt>
					<dd>
						{formatInteger(request.output_tokens)}
						{#if request.reasoning_tokens}
							<span class="muted">of which {formatInteger(request.reasoning_tokens)} reasoning</span
							>
						{/if}
					</dd>
					<dt>Counts come from</dt>
					<dd>{request.usage_source}</dd>
					<dt>Cost</dt>
					<dd>
						{#if request.cost === null || request.cost === undefined}
							<span class="muted"
								>{request.price_missing
									? 'No price is known for this model'
									: 'Not calculated'}</span
							>
						{:else}
							{formatCost(request.cost, request.currency)}
							<span class="muted">price version {request.price_version}</span>
						{/if}
					</dd>
					<dt>Audit record</dt>
					<dd class="mono">{request.audit_id || '–'}</dd>
				</dl>
			</section>
		</div>
	{/if}
</div>

<style>
	.columns {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(19rem, 1fr));
		gap: var(--space-4);
		align-items: start;
	}

	.tag {
		display: inline-block;
		margin: 0 var(--space-1) var(--space-1) 0;
		padding: 0 var(--space-2);
		border: 1px solid var(--border);
		border-radius: 999px;
		font-size: var(--text-xs);
	}
</style>
