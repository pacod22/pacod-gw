<script lang="ts">
	import BarChart from '#lib/components/BarChart.svelte';
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
	const delay = $derived(data.delay.data);
	const summary = $derived((delay?.summary ?? {}) as Row);
	const bucket = $derived(data.delay.filters.bucket ?? 'hour');
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
	/>

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
			<p class="muted small">Figures of the instance {live.instance_id}, from its own counters.</p>
		{/if}
	</section>

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
						'instance_id'
					].map((value) => ({ value, label: value.replaceAll('_', ' ') }))
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
				/>
			</div>
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
