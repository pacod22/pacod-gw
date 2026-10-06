/**
 * The ranking report (`/analytics/ranking`), prepared for its page: what each metric means,
 * how its values are written, the leader of each metric and the bars of the overall score.
 *
 * The analytics service ranks and scores; nothing here re-ranks. A metric's rank is among the
 * groups with enough calls that have a value for it, and the overall score weighs the metric
 * scores by the preset's weights (API.md, `GET /reports/ranking`).
 */
import { formatCost, formatMs } from '#lib/format.ts';

export type MetricKey =
	'success_rate' | 'first_byte_p50_ms' | 'first_byte_p95_ms' | 'output_ms_per_token' | 'cost';

export interface Measure {
	value: number | null;
	rank: number | null;
	score: number | null;
}

export interface RankingInfo {
	rate_limited_share: number;
	timeout_share: number;
	cache_read_share: number | null;
	price_coverage: number;
	estimated_share: number;
	avg_attempts: number;
	cost_per_request: number | null;
	output_tokens_per_second: number | null;
	currency: string | null;
	mixed_currencies: boolean;
}

export interface RankingRow {
	rank: number | null;
	score: number | null;
	ranked: boolean;
	provider: string;
	model: string | null;
	requests: number;
	eligible: number;
	metrics: Record<MetricKey, Measure>;
	info: RankingInfo;
}

export interface Ranking {
	kind: 'chat' | 'speech';
	by: 'model' | 'provider';
	preset: string;
	min_requests: number;
	currency: string | null;
	weights: Record<MetricKey, number>;
	rows: RankingRow[];
}

export interface MetricDefinition {
	key: MetricKey;
	label: string;
	/** Which way is better. */
	better: 'higher' | 'lower';
	/** Measured for chat calls only. */
	chatOnly?: boolean;
	help: string;
}

/** The ranked metrics, in the order of the preset weights. */
export const METRICS: readonly MetricDefinition[] = [
	{
		key: 'success_rate',
		label: 'Success',
		better: 'higher',
		help: 'Successful calls of those that reached the provider.'
	},
	{
		key: 'first_byte_p50_ms',
		label: 'First byte p50',
		better: 'lower',
		help: "Median time from the connection to the provider's response head, successful calls."
	},
	{
		key: 'first_byte_p95_ms',
		label: 'First byte p95',
		better: 'lower',
		help: '95th percentile of the same: how slow the slow calls are.'
	},
	{
		key: 'output_ms_per_token',
		label: 'Output per token',
		better: 'lower',
		chatOnly: true,
		help: 'Streaming time per output token, successful streamed calls.'
	},
	{
		key: 'cost',
		label: 'Cost',
		better: 'lower',
		help: 'Chat: per million tokens. Speech: per call. Priced calls in one currency only.'
	}
];

export const PRESETS = [
	{ value: 'balanced', label: 'Balanced' },
	{ value: 'speed', label: 'Speed' },
	{ value: 'cost', label: 'Cost' },
	{ value: 'reliability', label: 'Reliability' }
];

export const KINDS = [
	{ value: 'chat', label: 'Chat' },
	{ value: 'speech', label: 'Speech' }
];

export const BY = [
	{ value: 'model', label: 'Provider and model' },
	{ value: 'provider', label: 'Provider' }
];

/** The metrics a kind of call is ranked on. */
export function metricsOf(kind: string): MetricDefinition[] {
	return METRICS.filter((metric) => kind === 'chat' || !metric.chatOnly);
}

const isObject = (value: unknown): value is Record<string, unknown> =>
	typeof value === 'object' && value !== null;

/** The report as the page reads it, or `null` when it is not one. */
export function rankingOf(data: unknown): Ranking | null {
	if (!isObject(data) || !Array.isArray(data.rows) || !isObject(data.weights)) return null;
	return data as unknown as Ranking;
}

/**
 * `gemini / google/gemini-2.5-flash`, or the provider alone when providers are ranked. A
 * model whose name already starts with its provider (`anthropic/claude-sonnet-5-5`) is
 * written alone.
 */
export function nameOf(row: Pick<RankingRow, 'provider' | 'model'>): string {
	const provider = row.provider || 'No provider';
	if (row.model === null) return provider;
	if (row.provider && row.model.startsWith(`${row.provider}/`)) return row.model;
	return `${provider} / ${row.model || 'no model'}`;
}

/** The model alone, for a figure; the provider goes into its note when the name lacks it. */
function figureOf(row: RankingRow, note: string): { value: string; note: string } {
	if (row.model === null || nameOf(row) === row.model) return { value: nameOf(row), note };
	return { value: row.model || 'no model', note: `${note} · ${row.provider || 'no provider'}` };
}

const PERCENT = new Intl.NumberFormat('en-GB', { style: 'percent', maximumFractionDigits: 1 });
const SCORE = new Intl.NumberFormat('en-GB', { maximumFractionDigits: 1 });
const ONE_DECIMAL = new Intl.NumberFormat('en-GB', { maximumFractionDigits: 1 });

export function formatShare(value: unknown): string {
	return typeof value === 'number' && Number.isFinite(value) ? PERCENT.format(value) : '';
}

export function formatScore(value: unknown): string {
	return typeof value === 'number' && Number.isFinite(value) ? SCORE.format(value) : '';
}

/** A metric's value as text: `97.5%`, `240 ms`, `1.20 USD / 1M tokens`. */
export function formatMetric(
	key: MetricKey,
	value: number | null,
	kind: string,
	currency: string | null
): string {
	if (value === null || !Number.isFinite(value)) return '';
	switch (key) {
		case 'success_rate':
			return formatShare(value);
		case 'first_byte_p50_ms':
		case 'first_byte_p95_ms':
		case 'output_ms_per_token':
			return formatMs(value);
		case 'cost':
			return `${formatCost(value, currency)} ${kind === 'speech' ? '/ call' : '/ 1M tokens'}`;
	}
}

/**
 * A metric of a row as a table cell: the value and its rank, `240 ms · #2`. A cost in another
 * currency than the ranked one is shown with that currency and no rank.
 */
export function metricCell(row: RankingRow, key: MetricKey, ranking: Ranking): string {
	const measure = row.metrics[key];
	const currency = key === 'cost' ? (row.info.currency ?? ranking.currency) : null;
	const value = formatMetric(key, measure.value, ranking.kind, currency);
	if (value === '') return key === 'cost' && row.info.mixed_currencies ? 'mixed currencies' : '';
	return measure.rank === null ? value : `${value} · #${measure.rank}`;
}

export interface Leader {
	label: string;
	value: string;
	note?: string;
}

/** The best group overall and per metric: the first row, in the report's order, ranked 1. */
export function leaders(ranking: Ranking): Leader[] {
	const ranked = ranking.rows.filter((row) => row.ranked);
	const best = ranked[0];
	const out: Leader[] = [
		best
			? { label: 'Best overall', ...figureOf(best, `Score ${formatScore(best.score)} of 100`) }
			: { label: 'Best overall', value: '', note: 'Nothing ranked yet' }
	];
	const titles: Partial<Record<MetricKey, string>> = {
		success_rate: 'Most reliable',
		first_byte_p50_ms: 'Fastest first byte',
		output_ms_per_token: 'Fastest output',
		cost: 'Cheapest'
	};
	for (const metric of metricsOf(ranking.kind)) {
		const label = titles[metric.key];
		if (!label) continue;
		const leader = ranked.find((row) => row.metrics[metric.key].rank === 1);
		const value = leader?.metrics[metric.key].value ?? null;
		out.push(
			leader
				? {
						label,
						...figureOf(leader, formatMetric(metric.key, value, ranking.kind, ranking.currency))
					}
				: { label, value: '', note: 'Not measured' }
		);
	}
	return out;
}

export interface ScoreBar {
	name: string;
	score: number;
}

/** The overall scores of the best `top` ranked groups, best first. */
export function scoreBars(ranking: Ranking, top = 10): ScoreBar[] {
	return ranking.rows
		.filter((row) => row.ranked && row.score !== null)
		.slice(0, top)
		.map((row) => ({ name: nameOf(row), score: row.score ?? 0 }));
}

/** `Success 30%, first byte p50 15%, …`: the weights of the preset, for the note. */
export function weightsText(ranking: Ranking): string {
	return metricsOf(ranking.kind)
		.map((metric, index) => {
			const label = index === 0 ? metric.label : metric.label.toLowerCase();
			return `${label} ${formatShare(ranking.weights[metric.key] ?? 0)}`;
		})
		.join(', ');
}

/** Tokens per second, `85.2 tokens/s`. */
export function formatTokensPerSecond(value: unknown): string {
	return typeof value === 'number' && Number.isFinite(value)
		? `${ONE_DECIMAL.format(value)} tokens/s`
		: '';
}

/** The page's query with another preset: the switches above the scores are links. */
export function presetSearch(search: string, preset: string): string {
	const params = new URLSearchParams(search);
	params.set('preset', preset);
	return `?${params}`;
}
