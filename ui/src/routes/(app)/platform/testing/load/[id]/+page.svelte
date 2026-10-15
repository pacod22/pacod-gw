<script lang="ts">
	import { onMount } from 'svelte';
	import { enhance } from '$app/forms';
	import { invalidate } from '$app/navigation';
	import Badge from '#lib/components/Badge.svelte';
	import Figures from '#lib/components/Figures.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import OutcomeNotice from '#lib/components/OutcomeNotice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import TimeChart from '#lib/components/TimeChart.svelte';
	import { formatDateTime } from '#lib/format.ts';
	import {
		TARGET_TEXT,
		chartsOf,
		clock,
		isLoadFinished,
		loadStatusTone,
		ms,
		runningTotals,
		type LoadStatus,
		type Point
	} from '#lib/load.ts';
	import { formatDuration, runDuration, statusTone, type StepResult } from '#lib/testing.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();

	const run = $derived(data.run);
	/** The seconds so far: the stored ones, then each one as the tester adds it up. */
	let points: Point[] = $state([]);
	let status: LoadStatus | null = $state(null);
	let agentStates: Record<string, { state: string; calls: number }> = $state({});
	let now = $state(Date.now());

	$effect(() => {
		points = [...(data.run?.points ?? [])];
		status = data.run?.status ?? null;
		agentStates = {};
	});

	const current = $derived(status ?? run?.status ?? 'queued');
	const finished = $derived(isLoadFinished(current));
	// While it runs, the charts show the last fifteen minutes; afterwards everything.
	const shown = $derived(finished ? points : points.slice(-900));
	const charts = $derived(run ? chartsOf(shown, run.target, run.request.load.mode) : []);
	const totals = $derived(runningTotals(points));
	const s = $derived(run?.summary ?? null);
	const unit = $derived(run?.request.load.mode === 'rate' ? 'calls a second' : 'calls at a time');

	onMount(() => {
		if (!data.run || isLoadFinished(data.run.status)) return;
		const source = new EventSource(`/platform/testing/load/${data.run.id}/events`);
		source.addEventListener('point', (e) => {
			const item = JSON.parse((e as MessageEvent).data);
			const p: Point = item.point;
			if (!points.some((q) => q.t === p.t)) points.push(p);
			for (const a of item.agents ?? []) agentStates[a.name] = { state: a.state, calls: a.calls };
			status = item.status;
		});
		source.addEventListener('status', (e) => {
			status = JSON.parse((e as MessageEvent).data).status;
		});
		source.addEventListener('end', () => {
			source.close();
			void invalidate('app:testing-load');
		});
		source.addEventListener('error', () => {
			if (source.readyState === EventSource.CLOSED) void invalidate('app:testing-load');
		});
		const tick = setInterval(() => (now = Date.now()), 1000);
		return () => {
			source.close();
			clearInterval(tick);
		};
	});

	const pct = (part: number, whole: number) =>
		whole ? `${((100 * part) / whole).toFixed(part === whole ? 0 : 1)}%` : '–';
	const n = (v: number) => v.toLocaleString('en-GB');
	const open = (st: string) => st === 'failed' || st === 'error';

	const figures = $derived(
		s
			? [
					{ label: 'Calls', value: n(s.calls), note: `${pct(s.ok, s.calls)} successful` },
					{
						label: 'A second',
						value: n(s.rps_mean),
						note: `${n(s.rps_peak)} in the busiest second`
					},
					{ label: 'First byte', value: ms(s.ttfb.p50), note: `p99 ${ms(s.ttfb.p99)}` },
					{ label: 'Whole call', value: ms(s.total.p50), note: `p99 ${ms(s.total.p99)}` },
					{
						label: 'Gateway delay',
						value: s.gw_delay ? ms(s.gw_delay.p50) : '',
						note: s.gw_delay ? `p99 ${ms(s.gw_delay.p99)}` : 'not reported by the target'
					},
					{
						label: 'Failed',
						value: n(s.failed),
						note: s.skipped ? `${n(s.skipped)} not started` : ''
					}
				]
			: [
					{
						label: 'Calls so far',
						value: n(totals.calls),
						note: `${pct(totals.ok, totals.calls)} successful`
					},
					{ label: 'Last second', value: n(totals.rate), note: 'calls finished' },
					{
						label: 'First byte, last second',
						value: ms(totals.ttfbP50),
						note: `p99 ${ms(totals.ttfbP99)}`
					},
					{ label: 'Busiest agent', value: `${totals.cpu.toFixed(0)}%`, note: 'of its CPUs' }
				]
	);

	function targetText(): string {
		if (!run) return '';
		const r = run.request;
		switch (run.target) {
			case 'mock':
				return `the mock behind this gateway (${run.scene.profile || 'profile'}), model ${run.scene.model || '–'}`;
			case 'live':
				return `model ${run.scene.model || r.live?.model_id || '–'} on this gateway`;
			case 'own':
				return `model ${r.own?.model} with the key ${r.own?.key_hint} on this gateway`;
			case 'external':
				return `${run.base_url}, model ${r.external?.model} with the key ${r.external?.key_hint}`;
		}
	}

	function stagesText(): string {
		if (!run) return '';
		return run.request.load.stages.map((st) => `${st.duration_s} s → ${st.target}`).join(', ');
	}
</script>

{#if !run}
	{#if data.problem}<ProblemNotice problem={data.problem} />{/if}
{:else}
	<PageHeader
		title={run.label ? `Load test: ${run.label}` : `Load test ${run.id}`}
		crumbs={[{ label: 'Load tests', href: '/platform/testing/load' }]}
		description="{TARGET_TEXT[run.target]}, started by {run.started_by.display}."
	>
		{#snippet actions()}
			{#if !finished}
				<form method="POST" action="?/cancel" use:enhance>
					<button type="submit" class="btn btn-danger">Cancel the load test</button>
				</form>
			{:else}
				<a class="btn" href="/platform/testing/load">New load test</a>
			{/if}
		{/snippet}
	</PageHeader>

	<OutcomeNotice outcome={form} always={['cancel']} />
	{#if data.problem}<ProblemNotice problem={data.problem} />{/if}

	<section class="card stack">
		<div class="row summary">
			<Badge value={current} tone={loadStatusTone(current)} />
			{#if !finished}<span class="muted small live">updating every second</span>{/if}
		</div>
		<dl class="facts">
			<dt>Calls go to</dt>
			<dd>{targetText()}</dd>
			<dt>Request</dt>
			<dd>
				{run.request.interface}, {run.request.mode} mode{run.request.stream ? ', streamed' : ''},
				{run.request.prompt_words} words in, at most {run.request.max_tokens} tokens out, HTTP/{run
					.request.http}
			</dd>
			<dt>Load</dt>
			<dd>
				{unit}: {stagesText()}{run.request.load.max_calls
					? `; at most ${n(run.request.load.max_calls)} calls`
					: ''}{run.request.load.max_in_flight
					? `; at most ${n(run.request.load.max_in_flight)} in flight`
					: ''}
			</dd>
			<dt>Started (UTC)</dt>
			<dd>{run.started_at ? formatDateTime(run.started_at, true) : 'Waiting'}</dd>
			<dt>Took</dt>
			<dd>{formatDuration(runDuration(run, now))}</dd>
		</dl>
		{#if run.error}
			<Notice tone="error" title="The load test ended early"><p>{run.error}</p></Notice>
		{/if}
		{#if run.warnings.length > 0}
			<Notice tone="warning" title="Read the figures with this in mind">
				<ul>
					{#each run.warnings as w, i (i)}<li>{w}</li>{/each}
				</ul>
			</Notice>
		{/if}
		{#if run.scene.leftovers.length > 0}
			<Notice
				tone="warning"
				title={run.request.keep_resources ? 'Kept for a look' : 'Not everything could be removed'}
			>
				<p>Remove these by hand when they are no longer needed:</p>
				<ul class="mono small">
					{#each run.scene.leftovers as path (path)}<li>{path}</li>{/each}
				</ul>
			</Notice>
		{/if}
	</section>

	<Figures {figures} />

	<div class="charts">
		{#each charts as chart (chart.title)}
			<figure class="card chart">
				<figcaption>{chart.title}</figcaption>
				<TimeChart
					label={chart.title}
					labels={chart.labels}
					series={chart.series}
					format={chart.format}
					fill={false}
					height={200}
					live={!finished}
					emptyText={chart.empty}
				/>
			</figure>
		{/each}
	</div>

	<section class="stack">
		<h2>Agents</h2>
		<div class="table-wrap">
			<table class="plain">
				<thead>
					<tr>
						<th>Agent</th>
						<th>State</th>
						<th class="num">Calls</th>
						<th class="num">CPU at most</th>
						<th class="num">Lag at most</th>
						<th>Call before the load</th>
					</tr>
				</thead>
				<tbody>
					{#each run.agents as a (a.name)}
						{@const live = agentStates[a.name]}
						{@const state = live?.state ?? a.state}
						<tr>
							<td
								><strong>{a.name}</strong>
								<span class="muted small block"
									>{run.target === 'external' ? run.base_url : a.target}</span
								></td
							>
							<td>
								<Badge value={state} tone={loadStatusTone(state)} />
								{#if a.ended && a.ended !== 'duration'}<span class="muted small"
										>{a.ended.replace('_', ' ')}</span
									>{/if}
								{#if a.error}<span class="bad small block">{a.error}</span>{/if}
							</td>
							<td class="num">{n(live?.calls ?? a.calls)}</td>
							<td class="num">{a.cpu_max ? `${a.cpu_max.toFixed(0)}% of ${a.cpus}` : '–'}</td>
							<td class="num">{a.lag_max_ms ? ms(a.lag_max_ms) : '–'}</td>
							<td>
								{#if a.probe}
									{a.probe.status ?? a.probe.error ?? '–'} in {ms(a.probe.total_ms)}
									{#if a.probe.body_excerpt}<span class="muted small block mono"
											>{a.probe.body_excerpt}</span
										>{/if}
								{:else}–{/if}
							</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
	</section>

	{#if s}
		<section class="stack">
			<h2>Answers</h2>
			<div class="table-wrap">
				<table class="plain">
					<thead><tr><th>Outcome</th><th class="num">Calls</th></tr></thead>
					<tbody>
						{#each Object.entries(s.status).sort( ([a], [b]) => a.localeCompare(b) ) as [code, count] (code)}
							<tr><td>Status {code}</td><td class="num">{n(count)}</td></tr>
						{/each}
						{#each [['connect', 'Could not connect'], ['timeout', 'Timed out'], ['body', 'The answer broke off'], ['other', 'Failed otherwise'], ['cancelled', 'Stopped with the run']] as const as [key, label] (key)}
							{#if s.errors[key]}<tr><td>{label}</td><td class="num">{n(s.errors[key])}</td></tr
								>{/if}
						{/each}
						{#if s.skipped}<tr
								><td>Not started (max_in_flight)</td><td class="num">{n(s.skipped)}</td></tr
							>{/if}
					</tbody>
				</table>
			</div>
			<p class="muted small">
				{n(s.calls)} calls in {clock(s.duration_s)}; latencies are of the successful calls, read by
				the agents (first byte, whole call) and stated by the gateway (x-gw-delay-ms).
			</p>
		</section>
	{/if}

	{#if run.scene.setup.length > 0 || run.scene.teardown.length > 0}
		<section class="stack">
			<h2>Set-up and clean-up</h2>
			{#if run.scene.setup.length > 0}
				<details class="phase" open={run.scene.setup.some((st) => open(st.status))}>
					<summary>Set-up: {run.scene.setup.length} steps</summary>
					{@render steps(run.scene.setup)}
				</details>
			{/if}
			{#if run.scene.teardown.length > 0}
				<details class="phase" open={run.scene.leftovers.length > 0}>
					<summary>
						Clean-up: {run.scene.teardown.length - run.scene.leftovers.length} of {run.scene
							.teardown.length} removed
					</summary>
					{@render steps(run.scene.teardown)}
				</details>
			{/if}
		</section>
	{/if}
{/if}

{#snippet steps(list: StepResult[])}
	<ol class="steps">
		{#each list as step, i (i)}
			<li>
				<Badge value={step.status} tone={statusTone(step.status)} />
				<span>{step.name}</span>
				{#if step.detail}<span class="muted small">{step.detail}</span>{/if}
				{#if step.failures.length > 0}
					<ul class="bad small">
						{#each step.failures as failure, j (j)}<li>{failure}</li>{/each}
					</ul>
				{/if}
			</li>
		{/each}
	</ol>
{/snippet}

<style>
	.summary {
		align-items: center;
		gap: var(--space-3);
	}

	.live::before {
		display: inline-block;
		width: 0.5rem;
		height: 0.5rem;
		margin-right: var(--space-1);
		border-radius: 50%;
		background: var(--warning);
		content: '';
	}

	.charts {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(22rem, 1fr));
		gap: var(--space-4);
	}

	.chart {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		min-width: 0;
		margin: 0;
	}

	figcaption {
		font-size: var(--text-sm);
		font-weight: 650;
	}

	.table-wrap {
		overflow-x: auto;
	}

	table.plain {
		width: 100%;
		border-collapse: collapse;
		font-size: var(--text-sm);
	}

	.plain th,
	.plain td {
		padding: var(--space-2) var(--space-3);
		border-bottom: 1px solid var(--border);
		text-align: left;
		vertical-align: top;
	}

	.plain th {
		color: var(--text-muted);
		font-weight: 500;
	}

	.num {
		text-align: right;
		font-variant-numeric: tabular-nums;
	}

	.plain th.num {
		text-align: right;
	}

	.block {
		display: block;
	}

	.bad {
		color: var(--danger);
	}

	.phase > summary {
		color: var(--text-muted);
		font-size: var(--text-sm);
		cursor: pointer;
	}

	.steps {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		margin: var(--space-2) 0 0;
		padding-left: var(--space-3);
		border-left: 2px solid var(--border);
		list-style: none;
		overflow-wrap: anywhere;
	}

	ul {
		padding-left: var(--space-5);
	}
</style>
