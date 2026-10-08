<script lang="ts">
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import { canReadAuditRecords } from '#lib/access.ts';
	import { formatDateTime } from '#lib/format.ts';
	import { bodyText, factsOf, partsOf, requestDiff } from '#lib/insight/audit.ts';
	import { requestTabs, SCOPE_PARAMS } from '#lib/nav.ts';
	import type { PageProps } from './$types';

	let { data, params }: PageProps = $props();

	const record = $derived(data.record);
	const parts = $derived(partsOf(record));
	const diff = $derived(requestDiff(parts));
	const changed = $derived(diff ? diff.filter((line) => line.type !== 'same').length : 0);
	const views = $derived(requestTabs(params.id, canReadAuditRecords(data.principal)));
	const anchor = (name: string) => `part-${name.replace(/[^a-z0-9]+/gi, '-')}`;

	function open(name: string) {
		const element = document.getElementById(anchor(name));
		if (element instanceof HTMLDetailsElement) element.open = true;
	}
</script>

<div class="stack-lg">
	<PageHeader
		title="Audit record of request {params.id}"
		description="The call in full, as audit mode recorded it: what the client sent, what the gateway sent on and received, and what it answered. Opening this page is noted in the audit log."
	/>
	<Tabs label="Request" tabs={views} carry={SCOPE_PARAMS} variant="pills" />

	{#if data.notAudited}
		<div class="card stack">
			<p><strong>This call was not recorded in full.</strong></p>
			<p class="muted">
				Audit mode was not on for it, or its record has passed its retention period. The call's
				usage and timing are on the Call tab. Audit mode is switched on in the settings of the
				workspace or on a template.
			</p>
		</div>
	{:else if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if record}
		<section class="card stack">
			<h2>Record</h2>
			<dl class="facts">
				<dt>Audit id</dt>
				<dd class="mono">{record.audit_id}</dd>
				<dt>Recorded (UTC)</dt>
				<dd>{formatDateTime(record.timestamp, true)}</dd>
				<dt>Kept until (UTC)</dt>
				<dd>{formatDateTime(record.expires_at, true)}</dd>
				<dt>Complete</dt>
				<dd>{record.truncated ? 'No: a body was cut at the size limit' : 'Yes'}</dd>
			</dl>
		</section>

		<section class="card stack">
			<h2>What the gateway decided</h2>
			<dl class="facts">
				{#each factsOf(record.resolution) as fact (fact.label)}
					<dt>{fact.label}</dt>
					<dd>{fact.text}</dd>
				{/each}
			</dl>
		</section>

		<section class="card stack">
			<h2>What the gateway changed</h2>
			{#if !diff}
				<p class="muted">The request bodies cannot be compared as text.</p>
			{:else if changed === 0}
				<p>The provider received the body exactly as the client sent it.</p>
			{:else}
				<p class="muted">
					The client's request body against the body sent to the provider, line by line:
					<span class="removed-key">removed</span> lines were in the client's request only,
					<span class="added-key">added</span> lines in the provider's.
				</p>
				<pre class="diff">{#each diff as line, index (index)}<span class={line.type}
							>{line.type === 'removed' ? '- ' : line.type === 'added' ? '+ ' : '  '}{line.text}
</span>{/each}</pre>
			{/if}
		</section>

		<section class="stack">
			<h2>The four parts</h2>
			{#each parts as named (named.name)}
				{@const body = bodyText(parts, named)}
				<details class="card part" id={anchor(named.name)}>
					<summary>
						<strong>{named.title}</strong>
						<span class="muted">
							{named.part.method}
							{named.part.url}
							{named.part.status ? `HTTP ${named.part.status}` : ''} · {named.part.body_bytes} bytes
						</span>
					</summary>
					<div class="stack">
						<div class="table-wrap">
							<table>
								<caption class="visually-hidden">Headers of: {named.title}</caption>
								<thead><tr><th scope="col">Header</th><th scope="col">Value</th></tr></thead>
								<tbody>
									{#each named.part.headers as [name, value], index (index)}
										<tr><td class="mono">{name}</td><td class="mono value">{value}</td></tr>
									{/each}
								</tbody>
							</table>
						</div>
						{#if named.part.same_as}
							<p>
								{body.note}
								<a href="#{anchor(named.part.same_as)}" onclick={() => open(named.part.same_as)}
									>Open that part</a
								>
							</p>
						{:else}
							{#if body.note}<p class="muted">{body.note}</p>{/if}
							{#if body.text}<pre class="body">{body.text}</pre>{:else}<p class="muted">
									No body.
								</p>{/if}
						{/if}
					</div>
				</details>
			{/each}
		</section>

		<div class="columns">
			<section class="card stack">
				<h2>Timeline</h2>
				<p class="muted small">Microseconds from the first byte of the request.</p>
				<dl class="facts">
					{#each factsOf(record.timeline) as fact (fact.label)}
						<dt>{fact.label.replace(/ us$/, '')}</dt>
						<dd>{fact.text}</dd>
					{/each}
				</dl>
			</section>
			<section class="card stack">
				<h2>Outcome</h2>
				<dl class="facts">
					{#each factsOf(record.outcome) as fact (fact.label)}
						<dt>{fact.label}</dt>
						<dd>{fact.text}</dd>
					{/each}
				</dl>
			</section>
		</div>
	{/if}
</div>

<style>
	pre {
		margin: 0;
		padding: var(--space-3);
		overflow-x: auto;
		border: 1px solid var(--border);
		border-radius: var(--radius);
		background: var(--surface-2);
		font-family: var(--font-mono);
		font-size: var(--text-sm);
		line-height: 1.45;
	}

	.diff .removed {
		background: var(--danger-soft);
		color: var(--danger);
	}

	.diff .added {
		background: var(--success-soft);
		color: var(--success);
	}

	.removed-key {
		color: var(--danger);
		font-weight: 600;
	}

	.added-key {
		color: var(--success);
		font-weight: 600;
	}

	summary {
		display: flex;
		flex-wrap: wrap;
		gap: var(--space-2) var(--space-3);
		cursor: pointer;
		overflow-wrap: anywhere;
	}

	.part[open] summary {
		margin-bottom: var(--space-3);
	}

	.value {
		white-space: normal;
		overflow-wrap: anywhere;
	}

	.columns {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(19rem, 1fr));
		gap: var(--space-4);
		align-items: start;
	}
</style>
