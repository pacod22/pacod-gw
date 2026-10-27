<script lang="ts">
	import { enhance } from '$app/forms';
	import Badge from '#lib/components/Badge.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import TimeChart from '#lib/components/TimeChart.svelte';
	import { cellRows, deltaMs, factRows, latencyBars, ms, overheadBars } from '#lib/benchmarks.ts';
	import { formatDateTime } from '#lib/format.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
	const r = $derived(data.report);
	const s = $derived(data.summary);
	const rows = $derived(cellRows(r));
	const facts = $derived(factRows([r]));
	const total = $derived(latencyBars(r, 'total'));
	const gw = $derived(latencyBars(r, 'gw_delay'));
	const overhead = $derived(overheadBars(r));
	const n = (v: number | null | undefined) =>
		v === null || v === undefined ? '–' : v.toLocaleString('en-GB', { maximumFractionDigits: 1 });
</script>

<div class="stack-lg">
	<div class="stack">
		<p class="eyebrow"><a href="/platform/testing/benchmarks">Benchmarks</a></p>
		<h1>{r.name}</h1>
		<p class="muted">
			{r.target.label} ({r.target.product.replaceAll('_', '-')}, {r.target.kind}) · made {formatDateTime(
				r.created_at
			)} by {r.producer.name}
			{r.producer.version}
			· {s.source === 'upload' ? 'uploaded' : 'published'}
			{formatDateTime(s.imported_at)} by {s.published_by_name}{#if s.run_by}
				· run by {s.run_by}{/if}
		</p>
		{#if r.description}<p>{r.description}</p>{/if}
	</div>
	{#if form?.message}<Notice tone="error">{form.message}</Notice>{/if}

	<Notice
		tone={r.measurement.class === 'capacity' ? 'success' : 'info'}
		title={r.measurement.class === 'capacity'
			? 'These figures show capacity'
			: 'These figures show behaviour, not capacity'}
	>
		{#if r.measurement.reasons.length}<ul>
				{#each r.measurement.reasons as reason (reason)}<li>{reason}</li>{/each}
			</ul>{/if}
		{#if r.measurement.class !== 'capacity'}<p>
				An agent in the gateway's stack takes CPU from the gateway, and Docker's network adds time:
				for capacity, run agents on other machines (docs/testing.md, 9.6).
			</p>{/if}
	</Notice>

	<div class="grid">
		<section class="card stack">
			<h2>What was measured</h2>
			<dl class="kv">
				{#each facts as f (f.name)}<dt>{f.label}</dt>
					<dd>{f.values[0]} <span class="muted small">{f.sources[0]}</span></dd>{/each}
				<dt>Agents</dt>
				<dd>
					{(r.agents ?? [])
						.map(
							(a) =>
								`${a.name} (${a.placement.replace('_', ' ')}${a.cpus ? `, ${a.cpus} CPUs` : ''})`
						)
						.join(', ') || '–'}
				</dd>
			</dl>
		</section>
		<section class="card stack">
			<h2>The report</h2>
			<dl class="kv">
				<dt>Cells</dt>
				<dd>{r.cells.length}</dd>
				<dt>Schema</dt>
				<dd class="mono">{r.schema}{r.minor ? `, minor ${r.minor}` : ''}</dd>
				<dt>Built against</dt>
				<dd class="mono small">{r.producer.gw_commit || '–'}</dd>
				<dt>Digest</dt>
				<dd class="mono small">{s.digest.slice(0, 16)}…</dd>
			</dl>
			<div class="row">
				<a class="btn" href="/platform/testing/benchmarks/{s.id}/download" download>Download</a>
				{#if data.benchUrl}<a class="btn btn-quiet" href={data.benchUrl}>Open pacod-benchmark</a
					>{/if}
				<form method="POST" action="?/delete" use:enhance>
					<button class="btn btn-danger" type="submit">Delete</button>
				</form>
			</div>
		</section>
	</div>

	<section class="card stack">
		<h2>Results</h2>
		<div class="charts">
			<figure>
				<figcaption>Whole call</figcaption>
				<TimeChart
					label="Whole call"
					labels={total.labels}
					series={total.series}
					format={ms}
					height={240}
				/>
			</figure>
			<figure>
				<figcaption>Overhead against the baseline</figcaption>
				<TimeChart
					label="Overhead against the baseline"
					labels={overhead.labels}
					series={overhead.series}
					format={ms}
					height={240}
					emptyText="No baseline ran in this report."
				/>
			</figure>
			<figure>
				<figcaption>The gateway's own delay</figcaption>
				<TimeChart
					label="The gateway's own delay"
					labels={gw.labels}
					series={gw.series}
					format={ms}
					height={240}
					emptyText="The target sent no x-gw-delay-ms: it is not pacod-gw."
				/>
			</figure>
		</div>
		<div class="table-wrap">
			<table>
				<thead>
					<tr
						><th>Cell</th><th>Status</th><th>Calls/s</th><th>First byte p50</th><th>Whole p50</th
						><th>Whole p99</th><th>Gateway p99</th><th>Overhead p50 / p99</th><th>Flags</th></tr
					>
				</thead>
				<tbody>
					{#each rows as row (row.cell.index)}
						{@const c = row.cell}
						<tr class:baseline={c.kind === 'baseline'}>
							<td>{c.label}{c.rep ? ` (#${c.rep + 1})` : ''}</td>
							<td
								><Badge value={c.status} />{#if c.error}<br /><span class="muted small"
										>{c.error}</span
									>{/if}</td
							>
							<td>{n(row.steady?.rps_mean)}</td>
							<td>{ms(row.steady?.ttfb.p50)}</td>
							<td>{ms(row.steady?.total.p50)}</td>
							<td>{ms(row.steady?.total.p99)}</td>
							<td>{row.steady ? ms(row.steady.gw_delay?.p99) : '–'}</td>
							<td
								>{c.kind === 'baseline'
									? '–'
									: `${deltaMs(row.overheadP50)} / ${deltaMs(row.overheadP99)}`}</td
							>
							<td class="small">{(c.flags ?? []).map((f) => f.code).join(', ')}</td>
						</tr>
					{/each}
				</tbody>
			</table>
		</div>
		<p class="muted small">
			Steady window: from the end of the warm-up to the end of the last stage. The overhead is the
			whole call minus the baseline's, the same load straight to the mock; it includes what the
			gateway's own figure leaves out, such as relaying a stream.
		</p>
	</section>
</div>

<style>
	.grid,
	.charts {
		display: grid;
		grid-template-columns: repeat(auto-fit, minmax(22rem, 1fr));
		gap: var(--space-5);
		align-items: start;
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

	.kv {
		display: grid;
		grid-template-columns: max-content minmax(0, 1fr);
		gap: var(--space-2) var(--space-4);
		margin: 0;
	}

	.kv dt {
		color: var(--text-muted);
	}

	.kv dd {
		margin: 0;
		overflow-wrap: anywhere;
	}

	tr.baseline td {
		color: var(--text-muted);
	}
</style>
