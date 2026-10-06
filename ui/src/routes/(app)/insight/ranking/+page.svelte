<script lang="ts">
	import DataTable from '#lib/components/DataTable.svelte';
	import Figures from '#lib/components/Figures.svelte';
	import PageHeader from '#lib/components/PageHeader.svelte';
	import ProblemNotice from '#lib/components/ProblemNotice.svelte';
	import ReportFilters from '#lib/components/ReportFilters.svelte';
	import type { Column } from '#lib/components/table.ts';
	import { formatInteger } from '#lib/format.ts';
	import {
		BY,
		KINDS,
		PRESETS,
		formatScore,
		formatShare,
		formatTokensPerSecond,
		leaders,
		metricCell,
		metricsOf,
		nameOf,
		presetSearch,
		rankingOf,
		scoreBars,
		weightsText,
		type Ranking,
		type RankingRow
	} from '#lib/insight/ranking.ts';
	import type { PageProps } from './$types';

	let { data }: PageProps = $props();

	const ranking = $derived(rankingOf(data.report.data));
	const preset = $derived(ranking?.preset ?? data.report.filters.preset ?? 'balanced');
	const ranked = $derived(ranking?.rows.filter((row) => row.ranked) ?? []);
	const unranked = $derived(ranking?.rows.filter((row) => !row.ranked) ?? []);
	const bars = $derived(ranking ? scoreBars(ranking) : []);
	const groupLabel = $derived(ranking?.by === 'provider' ? 'Provider' : 'Provider / model');

	/** The ranked metrics as columns: the value and its rank, sorted by the value. */
	function metricColumns(ranking: Ranking): Column<RankingRow>[] {
		return metricsOf(ranking.kind).map((metric) => ({
			key: metric.key,
			label: metric.label,
			kind: 'number',
			value: (row) => metricCell(row, metric.key, ranking),
			sortValue: (row) => row.metrics[metric.key].value
		}));
	}

	function columnsOf(ranking: Ranking, withRank: boolean): Column<RankingRow>[] {
		const name: Column<RankingRow> = {
			key: 'name',
			label: groupLabel,
			kind: 'code',
			value: nameOf
		};
		const calls: Column<RankingRow> = {
			key: 'eligible',
			label: 'Calls',
			kind: 'number',
			value: (row) => formatInteger(row.eligible),
			sortValue: (row) => row.eligible
		};
		const share = (key: 'rate_limited_share' | 'timeout_share' | 'price_coverage', label: string) =>
			({
				key,
				label,
				kind: 'number',
				value: (row) => formatShare(row.info[key]),
				sortValue: (row) => row.info[key]
			}) satisfies Column<RankingRow>;
		const info: Column<RankingRow>[] = [
			share('rate_limited_share', 'Rate limited'),
			share('timeout_share', 'Timeouts'),
			share('price_coverage', 'Priced'),
			...(ranking.kind === 'chat'
				? [
						{
							key: 'output_tokens_per_second',
							label: 'Output speed',
							kind: 'number',
							value: (row) => formatTokensPerSecond(row.info.output_tokens_per_second),
							sortValue: (row) => row.info.output_tokens_per_second
						} satisfies Column<RankingRow>
					]
				: [])
		];
		if (!withRank) return [name, calls, ...metricColumns(ranking), ...info];
		return [
			{
				key: 'rank',
				label: 'Rank',
				kind: 'number',
				value: (row) => (row.rank ? `#${row.rank}` : ''),
				sortValue: (row) => row.rank
			},
			name,
			{
				key: 'score',
				label: 'Score',
				kind: 'number',
				value: (row) => formatScore(row.score),
				sortValue: (row) => row.score
			},
			...metricColumns(ranking),
			calls,
			...info
		];
	}

	const rowKey = (row: RankingRow) => nameOf(row);
</script>

<div class="stack-lg">
	<PageHeader
		title="Ranking"
		description="Providers and models side by side: how reliably, how fast and how cheaply they serve. Every metric is ranked on its own, and a preset weighs them into one score."
	/>
	<ReportFilters
		values={data.report.filters}
		tagKeys={data.tagKeys}
		csv="/insight/ranking/export{data.search}"
		controls={[
			{ name: 'from', label: 'From (UTC)', type: 'time' },
			{ name: 'to', label: 'To (UTC)', type: 'time' },
			{ name: 'kind', label: 'Calls', type: 'select', options: KINDS },
			{ name: 'by', label: 'Rank', type: 'select', options: BY },
			{ name: 'preset', label: 'Weigh for', type: 'select', options: PRESETS },
			{ name: 'min_requests', label: 'Minimum calls' },
			{ name: 'provider', label: 'Provider' },
			{ name: 'model', label: 'Model' },
			{ name: 'template', label: 'Template' }
		]}
	/>

	{#if data.report.problem}
		<ProblemNotice problem={data.report.problem} />
	{:else if ranking}
		<Figures figures={leaders(ranking)} />

		<section class="card stack">
			<div class="chart-head">
				<h2 id="scores-title">Overall score, best {bars.length}</h2>
				<nav class="segments" aria-label="Weigh for">
					{#each PRESETS as option (option.value)}
						<a
							href={presetSearch(data.search, option.value)}
							aria-current={preset === option.value ? 'page' : undefined}>{option.label}</a
						>
					{/each}
				</nav>
			</div>
			{#if bars.length === 0}
				<p class="muted small">
					No {ranking.by === 'provider' ? 'provider' : 'model'} has {ranking.min_requests} calls in this
					range yet. Lower the minimum or widen the range.
				</p>
			{:else}
				<ol class="bars" aria-labelledby="scores-title">
					{#each bars as bar, index (bar.name)}
						<li title="{bar.name}: score {formatScore(bar.score)} of 100">
							<span class="place">#{index + 1}</span>
							<span class="name">{bar.name}</span>
							<span class="track" aria-hidden="true">
								<span class="fill" style:width="{Math.max(bar.score, 0.5)}%"></span>
							</span>
							<strong class="score">{formatScore(bar.score)}</strong>
						</li>
					{/each}
				</ol>
			{/if}
			<p class="muted small">
				Weights for {PRESETS.find((p) => p.value === preset)?.label.toLowerCase() ?? preset}:
				{weightsText(ranking)}. A metric without a value drops out of a group's score.
			</p>
		</section>

		<section class="stack">
			<h2>Ranked</h2>
			{#if ranked.length === 0}
				<div class="card muted">Nothing has enough calls to be ranked in this range.</div>
			{:else}
				<DataTable
					caption="Ranked {ranking.by === 'provider' ? 'providers' : 'models'}"
					columns={columnsOf(ranking, true)}
					rows={ranked}
					{rowKey}
					filter={ranked.length > 8}
				/>
			{/if}
		</section>

		{#if unranked.length > 0}
			<section class="stack">
				<h2>Too few calls to rank</h2>
				<p class="muted small">
					Fewer than {ranking.min_requests} calls reached the provider in this range. Their figures are
					shown, but not ranked.
				</p>
				<DataTable
					caption="Too few calls to rank"
					columns={columnsOf(ranking, false)}
					rows={unranked}
					{rowKey}
				/>
			</section>
		{/if}

		<section class="card stack small">
			<h2>How to read this</h2>
			<ul class="notes">
				<li>
					Only calls that reached the provider count: calls the gateway refused and calls the client
					cancelled are left out. With failover a call counts for the model that served it.
				</li>
				<li>
					First byte is the time from the connection to the provider's response head, not to the
					first token.
				</li>
				<li>
					Output per token runs from the provider's first byte to its last, so on a stream it also
					holds any wait before the first token.
				</li>
				<li>
					Cost uses priced calls only{ranking.currency ? `, in ${ranking.currency}` : ''}. A group
					priced in another currency shows its cost without a rank.
				</li>
				<li>
					A metric's score runs from 100 for the best to 0 for the worst; equal values share a rank.
				</li>
			</ul>
		</section>
	{/if}
</div>

<style>
	h2 {
		margin: 0;
	}

	.chart-head {
		display: flex;
		flex-wrap: wrap;
		align-items: center;
		justify-content: space-between;
		gap: var(--space-3);
	}

	.chart-head h2,
	section.small h2 {
		font-size: var(--text-sm);
		font-weight: 650;
	}

	.segments {
		display: flex;
		gap: 2px;
		padding: 2px;
		border-radius: var(--radius);
		background: var(--surface-2);
	}

	.segments a {
		padding: 0.25rem 0.625rem;
		border-radius: calc(var(--radius) - 2px);
		color: var(--text-muted);
		font-size: var(--text-xs);
		font-weight: 600;
		text-decoration: none;
	}

	.segments a:hover {
		color: var(--text);
	}

	.segments [aria-current='page'] {
		background: var(--accent);
		color: var(--on-accent);
	}

	/* One bar per group: the score of 100 it reached. The number is written beside it. */
	.bars {
		display: flex;
		flex-direction: column;
		gap: var(--space-2);
		margin: 0;
		padding: 0;
		list-style: none;
	}

	.bars li {
		display: grid;
		grid-template-columns: 2.5rem minmax(8rem, 16rem) 1fr 3rem;
		align-items: center;
		gap: var(--space-3);
		font-size: var(--text-sm);
		font-variant-numeric: tabular-nums;
	}

	.place {
		color: var(--text-muted);
		font-family: var(--font-mono);
		font-size: var(--text-xs);
	}

	.name {
		overflow: hidden;
		font-family: var(--font-mono);
		font-size: var(--text-xs);
		text-overflow: ellipsis;
		white-space: nowrap;
	}

	.track {
		height: 0.5rem;
		border-radius: 999px;
		background: var(--accent-soft);
	}

	.fill {
		display: block;
		height: 100%;
		border-radius: inherit;
		background: var(--accent);
	}

	.score {
		text-align: right;
	}

	.notes {
		display: flex;
		flex-direction: column;
		gap: var(--space-1);
		margin: 0;
		padding-left: 1.25rem;
	}

	@media (max-width: 40rem) {
		.bars li {
			grid-template-columns: 2rem 1fr 2.5rem;
		}

		.track {
			grid-column: 2 / 4;
			grid-row: 2;
		}

		.score {
			grid-row: 1;
			grid-column: 3;
		}
	}
</style>
