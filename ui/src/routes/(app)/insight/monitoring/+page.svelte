<script lang="ts">
	import BarChart from '#lib/components/BarChart.svelte';
	import DataTable from '#lib/components/DataTable.svelte';
	import Figures from '#lib/components/Figures.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportFilters from '#lib/components/ReportFilters.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import RequestRows from '#lib/components/RequestRows.svelte';
	import StageBar from '#lib/components/StageBar.svelte';
	import { formatMs } from '#lib/format.ts';
	import { timingOf } from '#lib/insight/timing.ts';
	import { formatValue, rowsOf, type Row } from '#lib/report/format.ts';
	import { chartPoints, seriesRows } from '#lib/report/series.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const live = $derived(data.live as Row | null);
	const instances = $derived(data.live?.instances ?? []);
	type Instance = (typeof instances)[number];
	const perSecond = (v: number | undefined) => (v === undefined ? '–' : formatValue('n', v) || '0');
	const delayOf = (v: number | null | undefined) =>
		v === null || v === undefined ? '–' : formatMs(v);
	const delay = $derived(data.delay.data);
	/** The timing stages whose percentiles over time can be shown beside the delay's. */
	const STAGES = [
		['receive', 'Receiving the request'],
		['pre', 'Before the provider call'],
		['connect', 'Connecting to the provider'],
		['retry', 'Waiting for retries'],
		['upstream_first_byte', 'Provider, first byte'],
		['upstream_stream', 'Provider, streaming'],
		['post', 'After the provider answered'],
		['relay_max', 'Longest relay of a chunk'],
		['settle', 'Settling'],
		['total', 'Whole call']
	];
	const stage = $derived(data.delay.filters.stage ?? '');
	const stageLabel = $derived(STAGES.find(([value]) => value === stage)?.[1] ?? stage);
	const summary = $derived((delay?.summary ?? {}) as Row);
	const bucket = $derived(data.delay.filters.bucket ?? 'hour');
	/** The 99th-percentile targets per mode, as the installation's settings hold them. */
	const delayTargets = $derived(
		data.delayTargets
			? [
					{ label: 'Proxy target', value: data.delayTargets.proxy_ms },
					{ label: 'Managed target', value: data.delayTargets.managed_ms }
				]
			: []
	);
	const grouped = $derived(
		data.delay.filters.group_by && data.delay.filters.group_by !== 'none'
			? data.delay.filters.group_by
			: null
	);
	const tone = (severity: string) =>
		severity === 'critical' || severity === 'error'
			? 'error'
			: severity === 'warning'
				? 'warning'
				: 'info';
</script>

<div class="stack-lg">
	<PageHeader
		title="Monitoring"
		description="How the gateway is doing right now, the health of providers and models, open alerts, and the delay the gateway itself adds to calls."
	>
		{#snippet actions()}
			{#if data.grafanaUrl}
				<a class="btn" href={data.grafanaUrl} target="_blank" rel="noopener noreferrer">
					Open Grafana
				</a>
			{/if}
		{/snippet}
	</PageHeader>

	<section class="stack">
		<h2>Alerts</h2>
		{#if data.alertsProblem}
			<ProblemNotice problem={data.alertsProblem} />
		{:else if data.alerts.length === 0}
			<Notice tone="success"><p>No alert is open.</p></Notice>
		{:else}
			{#each data.alerts as alert (alert.id)}
				<Notice tone={tone(alert.severity)} title={alert.title}><p>{alert.detail}</p></Notice>
			{/each}
		{/if}
	</section>

	<section class="stack">
		<h2>Right now</h2>
		{#if data.liveProblem}
			<ProblemNotice problem={data.liveProblem} />
		{:else if live}
			{#if data.instance}
				<p>
					The instance <span class="mono">{data.instance}</span> alone.
					<a href="/insight/monitoring">Show all instances</a>
				</p>
			{:else}
				<p>
					{instances.length === 1
						? 'The one running instance.'
						: `The sum of all ${instances.length} running instances.`}
				</p>
			{/if}
			<Figures
				figures={[
					{ label: 'Calls in progress', value: formatValue('in_flight', live.in_flight) },
					{ label: 'Calls per second', value: formatValue('n', live.requests_per_second) },
					{
						label: 'Errors per second',
						value: formatValue('n', live.errors_per_second),
						note: `Rejected per second: ${formatValue('n', live.rejected_per_second) || '0'}`
					},
					{
						label: 'Gateway delay, median',
						value: live.gateway_delay_p50_ms === null ? '–' : formatMs(live.gateway_delay_p50_ms),
						note:
							live.gateway_delay_p99_ms === null
								? 'No calls in the window'
								: `99th percentile ${formatMs(live.gateway_delay_p99_ms)}`
					},
					{
						label: 'Configuration',
						value: `Version ${formatValue('n', live.config_version)}`,
						note: `${formatValue('n', live.config_lag_versions) || '0'} versions behind`
					},
					{
						label: 'Waiting to be shipped',
						value: `${formatValue('n', live.spool_bytes) || '0'} bytes`,
						note: `Audit: ${formatValue('n', live.audit_spool_bytes) || '0'} bytes; dropped events ${formatValue('n', live.events_dropped) || '0'}, audit records ${formatValue('n', live.audit_records_dropped) || '0'}`
					}
				]}
			/>
			<p class="muted small">
				Each instance publishes its figures every five seconds; the instance that answered this page
				({live.instance_id}) adds its own current ones. Configuration shows the version that
				instance applied, and how far the furthest instance is behind.
			</p>
			{#if !data.instance}
				<DataTable
					caption="Running instances"
					rows={instances}
					rowKey={(i) => i.id}
					columns={[
						{
							key: 'id',
							label: 'Instance',
							kind: 'code',
							href: (i: Instance) => `/insight/monitoring?instance=${encodeURIComponent(i.id)}`
						},
						{ key: 'role', label: 'Role' },
						{ key: 'hostname', label: 'Host' },
						{
							key: 'calls',
							label: 'Calls/s',
							kind: 'number',
							value: (i: Instance) => perSecond(i.figures?.requests_per_second),
							sortValue: (i: Instance) => i.figures?.requests_per_second
						},
						{
							key: 'errors',
							label: 'Errors/s',
							kind: 'number',
							value: (i: Instance) => perSecond(i.figures?.errors_per_second),
							sortValue: (i: Instance) => i.figures?.errors_per_second
						},
						{
							key: 'in_flight',
							label: 'In progress',
							kind: 'number',
							value: (i: Instance) => i.figures?.in_flight ?? '–'
						},
						{
							key: 'p99',
							label: 'Delay, p99',
							kind: 'number',
							value: (i: Instance) => delayOf(i.figures?.gateway_delay_p99_ms),
							sortValue: (i: Instance) => i.figures?.gateway_delay_p99_ms
						},
						{
							key: 'lag',
							label: 'Versions behind',
							kind: 'number',
							value: (i: Instance) => i.figures?.config_lag_versions ?? '–'
						},
						{
							key: 'spool',
							label: 'Waiting to be shipped',
							kind: 'number',
							value: (i: Instance) =>
								i.figures ? `${formatValue('n', i.figures.spool_bytes) || '0'} bytes` : '–',
							sortValue: (i: Instance) => i.figures?.spool_bytes
						},
						{
							key: 'dropped',
							label: 'Dropped events',
							kind: 'number',
							value: (i: Instance) => i.figures?.events_dropped ?? '–'
						}
					]}
				/>
				{#if instances.some((i) => !i.figures)}
					<p class="muted small">
						A dash: the instance has not published figures yet. It has just started, or it runs an
						older version.
					</p>
				{/if}
			{/if}
		{/if}
	</section>

	{#if data.routing.length > 0}
		<section class="stack">
			<h2>Routing</h2>
			<p class="muted">
				Circuit breakers, calls in progress and the time to the first byte of the models in template
				pools, for the models each instance has called. Every instance keeps its own: a breaker can
				be open on one and closed on another.
			</p>
			<DataTable
				caption="Routing state per instance"
				rows={data.routing}
				rowKey={(t) => `${t.instance_id} ${t.model} ${t.credential_id}`}
				columns={[
					{ key: 'instance_id', label: 'Instance', kind: 'code' },
					{ key: 'model', label: 'Model', kind: 'code' },
					{ key: 'provider', label: 'Provider' },
					{ key: 'breaker', label: 'Breaker', kind: 'status' },
					{ key: 'in_flight', label: 'In progress', kind: 'number' },
					{
						key: 'first_byte_ms',
						label: 'First byte',
						kind: 'number',
						value: (t) => delayOf(t.first_byte_ms),
						sortValue: (t) => t.first_byte_ms
					}
				]}
			/>
		</section>
	{/if}

	<section class="stack">
		<h2>Providers and models</h2>
		{#if data.healthProblem}
			<ProblemNotice problem={data.healthProblem} />
		{:else}
			<ReportTable
				caption="Health of providers and models"
				rows={data.health as Row[]}
				empty="No calls were made recently, so there is nothing to judge health by."
			/>
		{/if}
	</section>

	<section class="stack">
		<h2>Gateway delay</h2>
		<p class="muted">
			The time by which the gateway itself delayed answers: its work before the provider call and on
			the answer.
		</p>
		<ReportFilters
			values={data.delay.filters}
			csv="/insight/monitoring/export{data.search}"
			controls={[
				{ name: 'from', label: 'From (UTC)', type: 'time' },
				{ name: 'to', label: 'To (UTC)', type: 'time' },
				{
					name: 'bucket',
					label: 'Per',
					type: 'select',
					options: ['hour', 'day', 'week', 'month'].map((value) => ({ value, label: value }))
				},
				{
					name: 'group_by',
					label: 'Split by',
					type: 'select',
					options: [
						'none',
						'mode',
						'provider',
						'model',
						'template',
						'workspace_id',
						'instance_id',
						'agent_id',
						'user_id',
						'task',
						'complexity'
					].map((value) => ({ value, label: value.replaceAll('_', ' ') }))
				},
				{
					name: 'stage',
					label: 'Also a stage',
					type: 'select',
					options: [
						{ value: '', label: 'none' },
						...STAGES.map(([value, label]) => ({ value, label }))
					]
				}
			]}
		/>
		{#if data.delay.problem}
			<ProblemNotice problem={data.delay.problem} />
		{:else if delay}
			<Figures
				figures={[
					{ label: 'Calls', value: formatValue('requests', summary.requests) },
					{ label: 'Median', value: formatValue('x_ms', summary.gateway_delay_p50_ms) },
					{ label: '95th percentile', value: formatValue('x_ms', summary.gateway_delay_p95_ms) },
					{ label: '99th percentile', value: formatValue('x_ms', summary.gateway_delay_p99_ms) },
					{ label: 'Maximum', value: formatValue('x_ms', summary.gateway_delay_max_ms) }
				]}
			/>
			<div class="card">
				<BarChart
					title="Gateway delay, 99th percentile per {bucket}"
					points={chartPoints(delay, 'gateway_delay_p99_ms', bucket, 'max')}
					format={(v) => formatMs(v)}
					targets={delayTargets}
				/>
			</div>
			{#if stage}
				<div class="card">
					<BarChart
						title="{stageLabel}, 99th percentile per {bucket}"
						points={chartPoints(delay, 'stage_p99_ms', bucket, 'max')}
						format={(v) => formatMs(v)}
					/>
				</div>
			{/if}
			{#if delay.stage_averages}
				<div class="card stack">
					<h3>Average time per stage</h3>
					<StageBar timing={timingOf(delay.stage_averages as Row)} />
				</div>
			{/if}
			{#if grouped}
				<h3>By {grouped.replaceAll('_', ' ')}, per {bucket}</h3>
				<ReportTable
					caption="Gateway delay by {grouped}"
					rows={seriesRows(delay)}
					labels={{ group: grouped.replaceAll('_', ' ') }}
					empty="No calls in this range."
				/>
			{/if}
			<h3>Slowest calls</h3>
			<RequestRows
				caption="Slowest calls"
				rows={rowsOf(delay, 'slowest_requests')}
				empty="No calls in this range."
			/>
		{/if}
	</section>
</div>
