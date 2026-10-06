<script lang="ts">
	import { invalidate } from '$app/navigation';
	import Badge from '#lib/components/Badge.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import StageBar from '#lib/components/StageBar.svelte';
	import { formatCost, formatDateTime, formatInteger, formatMs } from '#lib/format.ts';
	import { bodyText, factsOf, partsOf, requestDiff } from '#lib/insight/audit.ts';
	import { timingOf } from '#lib/insight/timing.ts';
	import { formatDuration, gatewayHeadersOf, prettyBody } from '#lib/testing.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const message = $derived(data.message);
	const ex = $derived(message?.exchange);
	const log = $derived(data.log);
	const timing = $derived(log ? timingOf(log) : null);
	const parts = $derived(partsOf(data.audit));
	const diff = $derived(data.audit ? requestDiff(parts) : null);
	const changed = $derived(diff ? diff.filter((l) => l.type !== 'same').length : 0);
	const preview = $derived(message?.plan.preview ?? null);
	const waiting = $derived(data.logState === 'pending' || data.auditState === 'pending');
	const statusTone = (status: number) =>
		status >= 200 && status < 300 ? 'good' : status >= 400 || status === 0 ? 'bad' : 'warn';
	const otherHeaders = $derived(
		ex ? Object.entries(ex.headers).filter(([n]) => !n.toLowerCase().startsWith('x-gw-')) : []
	);
	const span = $derived(Math.max(ex?.total_ms ?? 0, 1));
	const list = (value: unknown) => (Array.isArray(value) ? value.map(String) : []);

	// The request log and the audit record arrive a little after the call: look again until
	// they are there, or the server side gives up.
	$effect(() => {
		if (!waiting) return;
		const timer = setTimeout(() => void invalidate('app:message'), 1500);
		return () => clearTimeout(timer);
	});
</script>

{#if !message}
	{#if data.problem}<ProblemNotice problem={data.problem} />{/if}
{:else if ex}
	<PageHeader
		title="Message {message.id}"
		crumbs={[{ label: 'Send a message', href: '/platform/testing/message' }]}
		description="Sent by {message.started_by.display} on {formatDateTime(
			message.created_at,
			true
		)} UTC."
	>
		{#snippet actions()}
			<a class="btn" href="/platform/testing/message?from={message.id}">Edit and send again</a>
			{#if message.request_id}
				<a class="btn btn-quiet" href="/insight/requests/{message.request_id}">Open in Insight</a>
			{/if}
		{/snippet}
	</PageHeader>

	<section class="card stack">
		<div class="row summary">
			<Badge
				value={ex.error ? 'no answer' : `HTTP ${ex.status}`}
				tone={ex.error ? 'bad' : statusTone(ex.status)}
			/>
			<span class="mono">{message.method} {message.path}</span>
			{#if ex.headers['x-gw-error-type']}<span class="muted"
					>{ex.headers['x-gw-error-type'].replaceAll('_', ' ')}</span
				>{/if}
		</div>
		{#if ex.error}<Notice tone="error" title="No complete answer"><p>{ex.error}</p></Notice>{/if}
		<dl class="facts">
			<dt>Sent as</dt>
			<dd>
				workspace <span class="mono">{message.ws}</span> of
				<span class="mono">{message.org}</span>
			</dd>
			<dt>Key</dt>
			<dd>
				{#if message.key.kind === 'temporary'}
					A temporary key <span class="mono">{message.key.id || '–'}</span>
					{#if message.key.removed}<span class="muted">– deleted after the call</span>{/if}
				{:else}
					Your own key <span class="mono">{message.key.id || ''}</span>
				{/if}
				{#if message.key.problem}<span class="warning small">{message.key.problem}</span>{/if}
			</dd>
			<dt>Request id</dt>
			<dd class="mono">{message.request_id || '–'}</dd>
			<dt>Response head after</dt>
			<dd>{formatDuration(ex.ttfb_ms)}</dd>
			<dt>First body byte after</dt>
			<dd>{formatDuration(ex.first_byte_ms)}</dd>
			<dt>Complete after</dt>
			<dd>
				{formatDuration(ex.total_ms)}
				<span class="muted"
					>{formatInteger(ex.body_bytes)} bytes in {ex.chunks}
					{ex.chunks === 1 ? 'chunk' : 'chunks'}</span
				>
			</dd>
		</dl>
	</section>

	<ol class="trace">
		<li class="step">
			<h2><span class="n">1</span> What was sent</h2>
			<div class="card stack">
				<p class="mono">{message.method} {message.url}</p>
				<table class="kv">
					<caption class="visually-hidden">Request headers</caption>
					<tbody>
						{#each Object.entries(message.request_headers) as [name, value] (name)}
							<tr><th scope="row" class="mono">{name}</th><td class="mono">{value}</td></tr>
						{/each}
					</tbody>
				</table>
				{#if message.body !== null}
					<details open>
						<summary class="small">Body, {formatInteger(message.body_bytes)} bytes</summary>
						<pre>{prettyBody(message.body)}</pre>
					</details>
				{/if}
			</div>
		</li>

		<li class="step">
			<h2><span class="n">2</span> What the gateway planned</h2>
			<div class="card stack">
				{#if message.plan.status === 'ok' && preview}
					<p class="muted small">
						The workspace's preview, asked just before the call. It runs as the workspace without
						the restrictions of a key; what happened is in the request log below.
					</p>
					<dl class="facts">
						<dt>Route</dt>
						<dd>{String(preview.route).replaceAll('_', ' ')}</dd>
						<dt>Provider</dt>
						<dd>{preview.provider}</dd>
						<dt>Model</dt>
						<dd class="mono">{preview.model_requested} → {preview.model_served}</dd>
						<dt>Template</dt>
						<dd>
							{preview.template
								? `${preview.template}, version ${preview.template_version}`
								: 'None'}
						</dd>
						<dt>Provider URL</dt>
						<dd class="mono">{preview.url}</dd>
						<dt>Left out</dt>
						<dd>{list(preview.dropped_params).join(', ') || 'Nothing'}</dd>
					</dl>
					{#each list(preview.warnings) as warning (warning)}<p class="warning small">
							{warning}
						</p>{/each}
					<details>
						<summary class="small">The body the provider would receive</summary>
						<pre>{JSON.stringify(preview.body, null, 2)}</pre>
					</details>
					{#if preview.routing && typeof preview.routing === 'object' && Object.keys(preview.routing).length}
						<details>
							<summary class="small">Routing</summary>
							<pre>{JSON.stringify(preview.routing, null, 2)}</pre>
						</details>
					{/if}
				{:else if message.plan.status === 'refused'}
					<Notice tone="warning" title="The gateway would refuse this">
						<p>
							{message.plan.detail}{message.plan.gateway_status
								? ` (an application gets HTTP ${message.plan.gateway_status})`
								: ''}
						</p>
					</Notice>
				{:else}
					<p class="muted">No preview: {message.plan.detail}.</p>
				{/if}
			</div>
		</li>

		<li class="step">
			<h2><span class="n">3</span> What came back</h2>
			<div class="card stack">
				{#if gatewayHeadersOf(ex.headers).length > 0}
					<table class="kv">
						<caption class="visually-hidden">The gateway's headers</caption>
						<tbody>
							{#each gatewayHeadersOf(ex.headers) as [name, value] (name)}
								<tr><th scope="row" class="mono">{name}</th><td class="mono">{value}</td></tr>
							{/each}
						</tbody>
					</table>
				{/if}
				{#if otherHeaders.length > 0}
					<details>
						<summary class="small">Other headers</summary>
						<table class="kv">
							<caption class="visually-hidden">Other response headers</caption>
							<tbody>
								{#each otherHeaders as [name, value] (name)}
									<tr><th scope="row" class="mono">{name}</th><td class="mono">{value}</td></tr>
								{/each}
							</tbody>
						</table>
					</details>
				{/if}

				{#if ex.stream && ex.events.length > 0}
					<h3>{ex.events.length} events over {formatDuration(ex.total_ms)}</h3>
					<div
						class="strip"
						role="img"
						aria-label="When the {ex.events.length} events arrived, from {formatDuration(
							ex.events[0].t_ms
						)} to {formatDuration(ex.events.at(-1)?.t_ms)}"
					>
						{#if ex.ttfb_ms !== null}
							<span
								class="mark head"
								style:left="{(ex.ttfb_ms / span) * 100}%"
								title="Response head at {formatDuration(ex.ttfb_ms)}"
							></span>
						{/if}
						{#each ex.events as e, i (i)}
							<span
								class="tick"
								style:left="{(e.t_ms / span) * 100}%"
								title="{e.event || 'data'} at {formatDuration(e.t_ms)}"
							></span>
						{/each}
					</div>
					<div class="axis small muted">
						<span>0</span><span>{formatDuration(span)}</span>
					</div>
					<details>
						<summary class="small">The events</summary>
						<div class="table-wrap">
							<table>
								<caption class="visually-hidden">Events of the stream</caption>
								<thead
									><tr
										><th scope="col" class="num">At</th><th scope="col">Event</th><th scope="col"
											>Data</th
										></tr
									></thead
								>
								<tbody>
									{#each ex.events as e, i (i)}
										<tr>
											<td class="num">{formatDuration(e.t_ms)}</td>
											<td class="mono">{e.event || '–'}</td>
											<td class="mono data">{e.data}</td>
										</tr>
									{/each}
								</tbody>
							</table>
						</div>
					</details>
				{/if}

				<details open={!ex.stream}>
					<summary class="small">
						Body, {formatInteger(ex.body_bytes)} bytes{ex.truncated
							? ' (cut: only the start was kept)'
							: ''}
					</summary>
					{#if ex.body}<pre>{ex.stream ? ex.body : prettyBody(ex.body)}</pre>{:else}<p
							class="muted"
						>
							No body.
						</p>{/if}
				</details>
			</div>
		</li>

		<li class="step">
			<h2><span class="n">4</span> The request log</h2>
			<div class="card stack">
				{#if data.logState === 'found' && log && timing}
					<div class="row summary">
						<Badge value={log.status} />
						<span class="muted">HTTP {log.http_status} · {log.mode} · {log.interface}</span>
						<a href="/insight/requests/{message.request_id}" class="small">The call in Insight</a>
					</div>
					<StageBar {timing} />
					<dl class="facts">
						<dt>Provider</dt>
						<dd>{log.provider || '–'}</dd>
						<dt>Model</dt>
						<dd class="mono">{log.model_requested || '–'} → {log.model_served || '–'}</dd>
						{#if log.template}
							<dt>Template</dt>
							<dd>{log.template} <span class="muted">version {log.template_version}</span></dd>
						{/if}
						{#if log.routing_scheme}
							<dt>Routing</dt>
							<dd>
								{log.routing_scheme}{log.routing_rule ? ` / ${log.routing_rule}` : ''}
								<span class="muted">{log.routing_source}</span>
							</dd>
						{/if}
						<dt>Attempts and failovers</dt>
						<dd>
							{log.attempts} attempts, {log.failovers} failovers
							{#if list(log.models_tried).length}<span class="muted"
									>– tried {list(log.models_tried).join(', ')}</span
								>{/if}
						</dd>
						<dt>Tokens</dt>
						<dd>
							{formatInteger(log.input_tokens)} in, {formatInteger(log.output_tokens)} out
							<span class="muted">({log.usage_source})</span>
						</dd>
						<dt>Cost</dt>
						<dd>
							{log.cost === null || log.cost === undefined
								? 'Not calculated'
								: formatCost(log.cost, log.currency)}
						</dd>
						<dt>Gateway delay</dt>
						<dd>{formatMs(log.gateway_delay_ms)} of {formatMs(log.total_ms)}</dd>
						{#if log.error_type}
							<dt>Error</dt>
							<dd>{log.error_type.replaceAll('_', ' ')}: {log.error_message}</dd>
						{/if}
					</dl>
				{:else if data.logState === 'pending'}
					<p class="muted waiting">
						Waiting for the request log: the analytics service records calls within a few seconds…
					</p>
				{:else if !message.request_id}
					<p class="muted">
						The answer carries no request id, so the gateway did not take the call as one it logs:
						it was refused before (no or a wrong key, a bad path) or never answered.
					</p>
				{:else if data.logState === 'missing'}
					<Notice tone="warning" title="Not in the request log">
						<p>
							The call did not appear within 45 seconds. Check that the analytics service is running
							(Platform → System).
						</p>
					</Notice>
				{:else if data.problem}
					<ProblemNotice problem={data.problem} />
				{/if}
			</div>
		</li>

		<li class="step">
			<h2><span class="n">5</span> The audit record</h2>
			<div class="card stack">
				{#if data.auditState === 'found' && data.audit}
					<div class="row summary">
						<span class="muted small">Reading the record is noted in the audit log.</span>
						<a href="/insight/requests/{message.request_id}/audit" class="small">The full record</a>
					</div>
					<h3>What the gateway changed</h3>
					{#if !diff}
						<p class="muted">The request bodies cannot be compared as text.</p>
					{:else if changed === 0}
						<p>The provider received the body exactly as the client sent it.</p>
					{:else}
						<pre class="diff">{#each diff as line, index (index)}<span class={line.type}
									>{line.type === 'removed' ? '- ' : line.type === 'added' ? '+ ' : '  '}{line.text}
</span>{/each}</pre>
					{/if}
					{#each parts as named (named.name)}
						{@const body = bodyText(parts, named)}
						<details>
							<summary class="small">
								<strong>{named.title}</strong>
								<span class="muted"
									>{named.part.method}
									{named.part.url}
									{named.part.status ? `HTTP ${named.part.status}` : ''} · {named.part.body_bytes} bytes</span
								>
							</summary>
							{#if body.note}<p class="muted small">{body.note}</p>{/if}
							{#if body.text}<pre>{body.text}</pre>{/if}
						</details>
					{/each}
					<div class="columns">
						<div>
							<h3>What the gateway decided</h3>
							<dl class="facts">
								{#each factsOf(data.audit.resolution) as fact (fact.label)}
									<dt>{fact.label}</dt>
									<dd>{fact.text}</dd>
								{/each}
							</dl>
						</div>
						<div>
							<h3>Outcome</h3>
							<dl class="facts">
								{#each factsOf(data.audit.outcome) as fact (fact.label)}
									<dt>{fact.label}</dt>
									<dd>{fact.text}</dd>
								{/each}
							</dl>
						</div>
					</div>
				{:else if data.auditState === 'pending' || (data.logState === 'pending' && message.request_headers['x-gw-audit'])}
					<p class="muted waiting">Waiting for the audit record…</p>
				{:else if data.auditState === 'not_audited'}
					<p class="muted">
						This call was not recorded in full.
						{message.request_headers['x-gw-audit']
							? 'It asked to be, but the audit mode of the workspace (or its organisation) does not take requests: set it to “on request” or “always” in the workspace settings.'
							: 'Tick “Ask for an audit record” to see the four bodies and what the gateway changed.'}
					</p>
				{:else if data.auditState === 'missing' && data.logState === 'found'}
					<Notice tone="warning" title="The audit record did not arrive">
						<p>
							The request log names one, but it could not be read in time. Try “The call in Insight”
							later.
						</p>
					</Notice>
				{:else}
					<p class="muted">Shown once the call is in the request log.</p>
				{/if}
			</div>
		</li>
	</ol>
{/if}

<style>
	.summary {
		align-items: center;
		gap: var(--space-3);
		overflow-wrap: anywhere;
	}

	.trace {
		display: flex;
		flex-direction: column;
		gap: var(--space-4);
		margin: var(--space-4) 0 0;
		padding: 0;
		list-style: none;
	}

	.step h2 {
		display: flex;
		align-items: center;
		gap: var(--space-2);
		margin-bottom: var(--space-2);
	}

	.n {
		display: inline-flex;
		align-items: center;
		justify-content: center;
		width: 1.6rem;
		height: 1.6rem;
		border-radius: 50%;
		background: var(--accent);
		color: var(--surface);
		font-size: var(--text-sm);
	}

	/* Header names as they were sent, not in the caps of a column heading. */
	.kv th {
		width: 16rem;
		font-family: var(--font-mono);
		letter-spacing: normal;
		text-transform: none;
		color: var(--text-muted);
		font-weight: 500;
		text-align: left;
		vertical-align: top;
	}

	.kv td,
	.kv th,
	.data {
		overflow-wrap: anywhere;
		white-space: normal;
	}

	.kv {
		font-size: var(--text-xs);
	}

	summary {
		cursor: pointer;
		overflow-wrap: anywhere;
	}

	pre {
		max-height: 28rem;
		margin: var(--space-2) 0 0;
		padding: var(--space-3);
		overflow: auto;
		border-radius: var(--radius);
		background: var(--surface-2);
		font-family: var(--font-mono);
		font-size: var(--text-xs);
		white-space: pre-wrap;
		overflow-wrap: anywhere;
	}

	.diff .removed {
		background: var(--danger-soft);
		color: var(--danger);
	}

	.diff .added {
		background: var(--success-soft);
		color: var(--success);
	}

	/* When the events of a stream arrived: one thin tick each, on one time axis. */
	.strip {
		position: relative;
		height: 2.25rem;
		border-bottom: 1px solid var(--border);
	}

	.tick,
	.mark {
		position: absolute;
		bottom: 0;
		width: 2px;
		border-radius: 2px 2px 0 0;
		transform: translateX(-1px);
	}

	.tick {
		height: 1.5rem;
		background: var(--accent);
	}

	/* A wider, invisible hit area for the tooltip. */
	.tick::before {
		position: absolute;
		inset: 0 -4px;
		content: '';
	}

	.mark.head {
		height: 2.25rem;
		background: var(--text-muted);
	}

	.axis {
		display: flex;
		justify-content: space-between;
	}

	.columns {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(19rem, 1fr));
		gap: var(--space-4);
		align-items: start;
	}

	.waiting::before {
		display: inline-block;
		width: 0.5rem;
		height: 0.5rem;
		margin-right: var(--space-2);
		border-radius: 50%;
		background: var(--warning);
		content: '';
		animation: pulse 1s ease-in-out infinite alternate;
	}

	@keyframes pulse {
		to {
			opacity: 0.3;
		}
	}

	@media (prefers-reduced-motion: reduce) {
		.waiting::before {
			animation: none;
		}
	}

	.warning {
		color: var(--warning);
	}
</style>
