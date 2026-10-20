<script lang="ts">
	import { invalidate } from '$app/navigation';
	import Figures from '#lib/components/Figures.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ReportFilters from '#lib/components/ReportFilters.svelte';
	import ReportTable from '#lib/components/ReportTable.svelte';
	import Tabs from '#lib/components/Tabs.svelte';
	import TimeChart from '#lib/components/TimeChart.svelte';
	import { formatDateTime } from '#lib/format.ts';
	import { BOARDS, METRIC_RANGES, type Filter, type Unit } from '#lib/metrics/boards.ts';
	import { formatUnit } from '#lib/metrics/series.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	/** Prometheus scrapes every 15 seconds; Grafana's dashboards refresh every 30. */
	const REFRESH_MS = 30_000;

	const board = $derived(BOARDS.find((b) => b.id === data.board) ?? BOARDS[0]);
	const search = $derived(new URLSearchParams(data.values).toString());
	const tabs = $derived(
		BOARDS.map((b) => ({ label: b.title, href: `/insight/metrics/${b.id}?${search}` }))
	);

	let paused = $state(false);
	$effect(() => {
		if (paused || !data.configured) return;
		const timer = setInterval(() => {
			if (!document.hidden) void invalidate('app:metrics');
		}, REFRESH_MS);
		return () => clearInterval(timer);
	});

	const options = (values: string[]) => values.map((value) => ({ value, label: value }));
	const brief = (items: { id: string; label: string }[]) =>
		items.map((item) => ({ value: item.id, label: item.label }));
	const controls = $derived([
		{
			name: 'range',
			label: 'Range',
			type: 'select' as const,
			options: METRIC_RANGES.map((r) => ({ value: r.id, label: r.label }))
		},
		...board.filters.map((filter: Filter) => {
			switch (filter) {
				case 'organisation':
					return {
						name: 'organisation_id',
						label: 'Organisation',
						type: 'select' as const,
						empty: 'All organisations',
						options: brief(data.orgs),
						resets: ['workspace_id']
					};
				case 'workspace':
					return {
						name: 'workspace_id',
						label: 'Workspace',
						type: 'select' as const,
						empty: 'All workspaces',
						options: brief(data.workspaces),
						...(data.values.organisation_id ? {} : { disabled: 'Choose an organisation' })
					};
				case 'provider':
					return {
						name: 'provider',
						label: 'Provider',
						type: 'select' as const,
						empty: 'All providers',
						options: options(data.choices.provider ?? []),
						...(board.filters.includes('model') ? { resets: ['model'] } : {})
					};
				case 'model':
					return {
						name: 'model',
						label: 'Model',
						type: 'select' as const,
						empty: 'All models',
						options: options(data.choices.model ?? [])
					};
				default:
					return {
						name: 'instance',
						label: 'Instance',
						type: 'select' as const,
						empty: 'All instances',
						options: options(data.choices.instance ?? [])
					};
			}
		})
	]);

	const formatter = (unit: Unit) => (value: number) => formatUnit(value, unit);
	const axisFormatter = (unit: Unit) => (value: number) =>
		unit === 'seconds' && value === 0 ? '0' : formatUnit(value, unit);
</script>

<div class="stack-lg">
	<PageHeader
		title="Metrics"
		description="What the gateway's instances report to Prometheus, as the Grafana dashboards show it: traffic, failures, latency, spend and the gateway's own state."
	/>
	<Tabs label="Metrics boards" {tabs} variant="pills" />

	{#if !data.configured}
		<Notice tone="info" title="Prometheus is not set up for the console">
			<p>
				Set <code>PROMETHEUS_URL</code> for the console to the Prometheus server that scrapes the
				gateway, for example <code>http://pacod-prometheus:9090</code>. The Grafana dashboards show
				the same figures.
			</p>
		</Notice>
	{:else}
		<div class="stack">
			<p class="muted small">{board.description}</p>
			<ReportFilters values={data.values} {controls} />
			<div class="refresh">
				<span class="muted small">
					Updated {formatDateTime(data.updated, true)} UTC{paused
						? ''
						: ' · refreshes every 30 seconds'}
				</span>
				<button type="button" class="btn" onclick={() => (paused = !paused)}>
					{paused ? 'Resume refreshing' : 'Pause refreshing'}
				</button>
			</div>
		</div>

		{#each data.sections as section, index (index)}
			{@const stats = section.panels.filter((p) => p.kind === 'stat')}
			<section class="stack">
				{#if section.title}<h2>{section.title}</h2>{/if}
				{#if stats.length > 0}
					<Figures
						figures={stats.map((p) => ({
							label: p.title,
							value: p.problem ? '–' : (p.value ?? '–'),
							note: p.problem ?? p.description
						}))}
					/>
				{/if}
				<div class="panels">
					{#each section.panels.filter((p) => p.kind !== 'stat') as panel (panel.title)}
						<figure class="card panel" class:wide={panel.kind === 'table'}>
							<figcaption>
								<span class="title">{panel.title}</span>
								<span class="muted small">{panel.description}</span>
							</figcaption>
							{#if panel.problem}
								<p class="problem small">{panel.problem}</p>
							{:else if panel.kind === 'line'}
								<TimeChart
									label={panel.title}
									labels={data.labels}
									series={panel.series ?? []}
									stacked={panel.stacked}
									lines
									fill={false}
									format={formatter(panel.unit)}
									axisFormat={axisFormatter(panel.unit)}
									height={240}
								/>
							{:else}
								<ReportTable
									caption={panel.title}
									rows={panel.rows ?? []}
									fields={panel.columns}
									empty="Nothing in this range."
								/>
							{/if}
						</figure>
					{/each}
				</div>
			</section>
		{/each}
	{/if}
</div>

<style>
	.refresh {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-3);
	}

	.panels {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(26rem, 1fr));
		gap: var(--space-4);
	}

	.panel {
		display: flex;
		flex-direction: column;
		gap: var(--space-3);
		min-width: 0;
		margin: 0;
	}

	/* A table needs the room of a whole row for its columns. */
	.panel.wide {
		grid-column: 1 / -1;
	}

	figcaption {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
	}

	.title {
		font-size: var(--text-sm);
		font-weight: 650;
	}

	.problem {
		color: var(--danger);
	}

	@media (max-width: 40rem) {
		.panels {
			grid-template-columns: minmax(0, 1fr);
		}
	}
</style>
