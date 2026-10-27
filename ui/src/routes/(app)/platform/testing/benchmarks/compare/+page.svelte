<script lang="ts">
	import Notice from '#lib/components/Notice.svelte';
	import TimeChart from '#lib/components/TimeChart.svelte';
	import { alignReports, deltaMs, factRows, ms } from '#lib/benchmarks.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();
	const reports = $derived(data.reports.map((r) => r.report));
	const names = $derived(data.reports.map((r) => `${r.report.name} (${r.report.target.label})`));
	const facts = $derived(factRows(reports));
	const rows = $derived(alignReports(reports));
	const colours = [
		'--series-1',
		'--series-2',
		'--series-3',
		'--series-4',
		'--series-5',
		'--series-6'
	];
	const bars = (pick: (row: (typeof rows)[number], i: number) => number | null) => ({
		labels: rows.map((r) => r.label),
		series: reports.map((_, i) => ({
			name: names[i],
			values: rows.map((row) => pick(row, i)),
			colour: colours[i % 6],
			bar: true
		}))
	});
	const p99 = $derived(bars((row, i) => row.cells[i]?.steady?.total.p99 ?? null));
	const overhead = $derived(bars((row, i) => row.cells[i]?.overheadP50 ?? null));
	const classes = $derived(new Set(reports.map((r) => r.measurement.class)));
</script>

<div class="stack-lg">
	<div class="stack">
		<p class="eyebrow"><a href="/platform/testing/benchmarks">Benchmarks</a></p>
		<h1>Compare reports</h1>
		<p class="muted">
			Only cells that ran the same configuration (the request, the mock behind the target, the load,
			the gateway's settings) share a row. What differs between the deployments is listed first.
		</p>
	</div>
	{#if classes.has('behaviour')}
		<Notice tone="info" title="Some figures show behaviour, not capacity"
			>A report whose agents ran in the gateway's stack, or were short of CPU, measures how the
			gateway behaves; across machines, compare such figures with care.</Notice
		>
	{/if}

	<section class="card stack">
		<h2>What each report measured</h2>
		<div class="table-wrap">
			<table>
				<thead
					><tr
						><th>Fact</th>{#each names as name (name)}<th>{name}</th>{/each}</tr
					></thead
				>
				<tbody>
					<tr
						><td>Target</td>{#each reports as r, i (i)}<td
								>{r.target.product.replaceAll('_', '-')}, {r.target.kind}</td
							>{/each}</tr
					>
					<tr
						><td>Figures</td>{#each reports as r, i (i)}<td>{r.measurement.class}</td>{/each}</tr
					>
					{#each facts as f (f.name)}
						<tr class:differs={f.differs}
							><td>{f.label}</td>{#each f.values as v, i (i)}<td
									>{v} <span class="muted small">{f.sources[i]}</span></td
								>{/each}</tr
						>
					{/each}
				</tbody>
			</table>
		</div>
	</section>

	{#if rows.length === 0}
		<Notice tone="info"
			>No configuration ran in two of these reports. A report of another product lines up with this
			gateway's only when it declares the same mock behind it.</Notice
		>
	{:else}
		<section class="card stack">
			<h2>The same configurations</h2>
			<div class="charts">
				<figure>
					<figcaption>Whole call p99</figcaption>
					<TimeChart
						label="Whole call p99"
						labels={p99.labels}
						series={p99.series}
						format={ms}
						height={260}
					/>
				</figure>
				<figure>
					<figcaption>Overhead against the baseline, p50</figcaption>
					<TimeChart
						label="Overhead p50"
						labels={overhead.labels}
						series={overhead.series}
						format={ms}
						height={260}
						emptyText="No report had a baseline."
					/>
				</figure>
			</div>
			<div class="table-wrap">
				<table>
					<thead
						><tr
							><th>Configuration</th>{#each names as name (name)}<th>{name}</th>{/each}</tr
						></thead
					>
					<tbody>
						{#each rows as row (row.key)}
							<tr>
								<td>{row.label}</td>
								{#each row.cells as c, i (i)}
									<td class="small">
										{#if c?.steady}
											{c.steady.rps_mean.toFixed(1)}/s · p50 {ms(c.steady.total.p50)} · p99 {ms(
												c.steady.total.p99
											)}<br />
											overhead {deltaMs(c.overheadP50)} · gateway p99 {ms(c.steady.gw_delay?.p99)}
										{:else}<span class="muted">–</span>{/if}
									</td>
								{/each}
							</tr>
						{/each}
					</tbody>
				</table>
			</div>
		</section>
	{/if}
</div>

<style>
	.charts {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(22rem, 1fr));
		gap: var(--space-5);
	}

	figure {
		margin: 0;
		min-width: 0;
	}

	figcaption {
		margin-bottom: var(--space-2);
		font-weight: 600;
		font-size: var(--text-sm);
	}

	tr.differs td {
		background: var(--warning-soft, transparent);
	}
</style>
