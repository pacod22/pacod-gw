<script lang="ts">
	import { enhance } from '$app/forms';
	import Badge from '#lib/components/Badge.svelte';
	import Notice from '#lib/components/Notice.svelte';
	import { formatDateTime } from '#lib/format.ts';
	import type { PageProps } from './$types';

	let { data, form }: PageProps = $props();
	let chosen: string[] = $state([]);
</script>

<div class="stack-lg">
	<div class="stack">
		<h1>Benchmarks</h1>
		<p class="muted">
			Reports of pacod-benchmark: a suite of load tests on this gateway or another target, in many
			configurations, each measured against a baseline straight to the mock. A benchmark publishes
			its reports here, or you upload a report file it made on another machine.
			{#if data.benchUrl}<a href={data.benchUrl}>Open pacod-benchmark</a>{/if}
		</p>
	</div>
	{#if data.problem}<Notice tone="error">{data.problem}</Notice>{/if}

	<div class="card table-wrap">
		<table>
			<thead>
				<tr
					><th><span class="visually-hidden">Compare</span></th><th>Report</th><th>Target</th><th
						>Gateway</th
					><th>Cells</th><th>Figures</th><th>From</th><th>Made</th></tr
				>
			</thead>
			<tbody>
				{#each data.reports as r (r.id)}
					<tr>
						<td
							><input
								type="checkbox"
								value={r.id}
								bind:group={chosen}
								aria-label="Compare this report"
							/></td
						>
						<td><a href="/platform/testing/benchmarks/{r.id}">{r.name}</a></td>
						<td
							>{r.target_label}<br /><span class="muted small"
								>{r.product.replaceAll('_', '-')}, {r.target_kind}</span
							></td
						>
						<td>{r.gateway_version || '–'}{r.layout ? `, ${r.layout}` : ''}</td>
						<td>{r.cells}</td>
						<td
							><Badge
								value={r.measurement_class}
								tone={r.measurement_class === 'capacity' ? 'good' : 'neutral'}
							/></td
						>
						<td class="small"
							>{r.source === 'upload' ? 'uploaded' : 'published'} by {r.published_by_name}{#if r.run_by}<br
								/><span class="muted">run by {r.run_by}</span>{/if}</td
						>
						<td class="muted small">{formatDateTime(r.created_at)}</td>
					</tr>
				{:else}
					<tr><td colspan="8" class="muted">No report yet.</td></tr>
				{/each}
			</tbody>
		</table>
	</div>
	<div class="row">
		<a
			class="btn"
			href={chosen.length >= 2
				? `/platform/testing/benchmarks/compare?ids=${chosen.join(',')}`
				: undefined}
			aria-disabled={chosen.length < 2}
			class:disabled={chosen.length < 2 || chosen.length > 6}
		>
			Compare {chosen.length || ''} reports
		</a>
	</div>

	<form
		class="card stack"
		method="POST"
		action="?/upload"
		enctype="multipart/form-data"
		use:enhance
	>
		<h2>Upload a report</h2>
		<p class="muted small">
			A report file (pacod-bench-report/1, at most 1 MiB) downloaded from pacod-benchmark or written
			by <code>pacod-bench run</code>. It is checked in full; the same report twice is kept once.
		</p>
		{#if form?.message}
			<Notice tone="error" title={form.message}>
				{#if form.problems?.length}<ul>
						{#each form.problems as p, i (i)}<li><code>{p.path}</code> {p.message}</li>{/each}
					</ul>{/if}
			</Notice>
		{/if}
		<input type="file" name="report" accept="application/json,.json" required />
		<div class="row"><button class="btn btn-primary" type="submit">Upload</button></div>
	</form>
</div>

<style>
	.disabled {
		opacity: 0.5;
		pointer-events: none;
	}
</style>
