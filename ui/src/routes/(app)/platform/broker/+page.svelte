<script lang="ts">
	import DataTable from '#lib/components/DataTable.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();
	const status = $derived(
		(data.status ?? {}) as {
			configured?: boolean;
			url?: string;
			reachable?: boolean;
			error?: string | null;
			resources?: number | null;
			linked_models?: number;
			pools?: number;
		}
	);
	const linked = $derived(status.linked_models ?? 0);
	const pools = $derived(status.pools ?? 0);
	const rows = $derived(
		data.linked.map((m) => ({
			...m,
			reputation: m.score === null ? m.label || '—' : `${m.score.toFixed(2)} (${m.label})`
		}))
	);
</script>

<div class="stack-lg">
	<PageHeader
		title="Broker"
		description="pacod-broker, the catalogue of what can be called, at what price and how well: what the core reads from it, and the repository models that are broker resources."
	/>

	{#if data.problem}
		<ProblemNotice problem={data.problem} />
	{:else if !status.configured}
		<Notice tone="info" title="No broker is set up">
			<p>
				Set <code>PGW_BROKER_URL</code> (and <code>PGW_BROKER_TOKEN</code>) for the core to take
				proposals from the broker, resolve templates' pools from it and serve <code>/mcp</code>.
				gw-broker, which offers templates on the broker, is set up on its own (runbook section 20).
			</p>
		</Notice>
	{:else}
		{#if status.reachable}
			<Notice tone="success">
				<p>
					The broker at <code>{status.url}</code> answers. {linked === 1
						? '1 model is'
						: `${linked} models are`} linked to its resources; {pools === 1
						? '1 template takes'
						: `${pools} templates take`} members from it.
				</p>
			</Notice>
		{:else}
			<Notice tone="warning" title="The broker does not answer">
				<p><code>{status.url}</code>: {status.error ?? 'no answer'}</p>
			</Notice>
		{/if}
		<p class="muted small">
			Models the broker lists at your providers appear in
			<a href="/platform/models/discovery">Discovery</a>, found by pacod-broker; nothing changes
			until you accept them.
		</p>
	{/if}

	<section class="stack" aria-labelledby="linked-heading">
		<h2 id="linked-heading">Linked models</h2>
		{#if rows.length === 0}
			<div class="card muted">
				No model of the repository is linked to a broker resource yet. Set its
				<code>broker_resource_id</code>, or accept a proposal from the broker.
			</div>
		{:else}
			<DataTable
				caption="Repository models that are broker resources"
				{rows}
				rowKey={(r) => r.id}
				columns={[
					{ key: 'name', label: 'Model', kind: 'code', href: (r) => `/platform/models/${r.id}` },
					{ key: 'state', label: 'State', kind: 'status' },
					{ key: 'resource', label: 'Broker resource', kind: 'code' },
					{ key: 'reputation', label: 'Reputation' },
					{ key: 'price', label: 'Price now' },
					{ key: 'live', label: 'Live' }
				]}
			/>
			{#if data.more > 0}<p class="muted small">And {data.more} more.</p>{/if}
		{/if}
	</section>
</div>
