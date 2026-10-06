import { describe, expect, it } from 'vitest';
import {
	formatMetric,
	leaders,
	metricCell,
	metricsOf,
	nameOf,
	presetSearch,
	rankingOf,
	scoreBars,
	weightsText,
	type Measure,
	type MetricKey,
	type Ranking,
	type RankingRow
} from './ranking';

const none: Measure = { value: null, rank: null, score: null };

function row(
	provider: string,
	model: string | null,
	rank: number | null,
	score: number | null,
	metrics: Partial<Record<MetricKey, Measure>>,
	currency: string | null = 'USD'
): RankingRow {
	return {
		rank,
		score,
		ranked: rank !== null,
		provider,
		model,
		requests: 40,
		eligible: 40,
		metrics: {
			success_rate: none,
			first_byte_p50_ms: none,
			first_byte_p95_ms: none,
			output_ms_per_token: none,
			cost: none,
			...metrics
		},
		info: {
			rate_limited_share: 0,
			timeout_share: 0,
			cache_read_share: null,
			price_coverage: 1,
			estimated_share: 0,
			avg_attempts: 1,
			cost_per_request: null,
			output_tokens_per_second: null,
			currency,
			mixed_currencies: false
		}
	};
}

const ranking: Ranking = {
	kind: 'chat',
	by: 'model',
	preset: 'balanced',
	min_requests: 20,
	currency: 'USD',
	weights: {
		success_rate: 0.3,
		first_byte_p50_ms: 0.15,
		first_byte_p95_ms: 0.15,
		output_ms_per_token: 0.15,
		cost: 0.25
	},
	rows: [
		row('google', 'mixed', 1, 100, {
			success_rate: { value: 1, rank: 1, score: 100 },
			first_byte_p50_ms: { value: 150, rank: 1, score: 100 }
		}),
		row('anthropic', 'fast', 2, 60, {
			success_rate: { value: 1, rank: 1, score: 100 },
			first_byte_p50_ms: { value: 200, rank: 2, score: 50 },
			output_ms_per_token: { value: 10, rank: 1, score: 100 },
			cost: { value: 2, rank: 2, score: 0 }
		}),
		row(
			'openai',
			'cheap',
			3,
			29.41,
			{
				success_rate: { value: 0.75, rank: 3, score: 0 },
				cost: { value: 1, rank: 1, score: 100 }
			},
			'USD'
		),
		row('small', 'tiny', null, null, { cost: { value: 0.5, rank: null, score: null } }, 'EUR')
	]
};

describe('the ranking page', () => {
	it('reads only what looks like a ranking', () => {
		expect(rankingOf(ranking)).toBe(ranking);
		expect(rankingOf({ rows: [] })).toBeNull();
		expect(rankingOf(null)).toBeNull();
	});

	it('names a group by provider and model, or by provider alone', () => {
		expect(nameOf({ provider: 'anthropic', model: 'fast' })).toBe('anthropic / fast');
		expect(nameOf({ provider: 'anthropic', model: null })).toBe('anthropic');
		expect(nameOf({ provider: '', model: '' })).toBe('No provider / no model');
		expect(nameOf({ provider: 'anthropic', model: 'anthropic/claude-haiku-4-5' })).toBe(
			'anthropic/claude-haiku-4-5'
		);
		expect(nameOf({ provider: 'gemini', model: 'google/gemini-2.5-flash' })).toBe(
			'gemini / google/gemini-2.5-flash'
		);
	});

	it('leaves the output speed out for speech', () => {
		expect(metricsOf('chat').map((m) => m.key)).toContain('output_ms_per_token');
		expect(metricsOf('speech').map((m) => m.key)).not.toContain('output_ms_per_token');
	});

	it('writes values with their unit and rank', () => {
		expect(formatMetric('success_rate', 0.975, 'chat', null)).toBe('97.5%');
		expect(formatMetric('first_byte_p50_ms', 240, 'chat', null)).toBe('240 ms');
		expect(formatMetric('cost', 1.2, 'chat', 'USD')).toBe('1.20 USD / 1M tokens');
		expect(formatMetric('cost', 0.015, 'speech', 'USD')).toBe('0.015 USD / call');
		expect(formatMetric('cost', null, 'chat', 'USD')).toBe('');

		expect(metricCell(ranking.rows[1], 'first_byte_p50_ms', ranking)).toBe('200 ms · #2');
		// Another currency: shown in it, without a rank.
		expect(metricCell(ranking.rows[3], 'cost', ranking)).toBe('0.50 EUR / 1M tokens');
		expect(metricCell(ranking.rows[0], 'cost', ranking)).toBe('');
		const mixed = { ...ranking.rows[0], info: { ...ranking.rows[0].info, mixed_currencies: true } };
		expect(metricCell(mixed, 'cost', ranking)).toBe('mixed currencies');
	});

	it('names the leader of each metric, the first in the order on a shared first place', () => {
		expect(leaders(ranking)).toEqual([
			{ label: 'Best overall', value: 'mixed', note: 'Score 100 of 100 · google' },
			{ label: 'Most reliable', value: 'mixed', note: '100% · google' },
			{ label: 'Fastest first byte', value: 'mixed', note: '150 ms · google' },
			{ label: 'Fastest output', value: 'fast', note: '10.0 ms · anthropic' },
			{ label: 'Cheapest', value: 'cheap', note: '1.00 USD / 1M tokens · openai' }
		]);
		const empty = leaders({ ...ranking, rows: [ranking.rows[3]] });
		expect(empty[0]).toEqual({ label: 'Best overall', value: '', note: 'Nothing ranked yet' });
		expect(empty[4]).toEqual({ label: 'Cheapest', value: '', note: 'Not measured' });
	});

	it('draws bars for the ranked groups only', () => {
		expect(scoreBars(ranking)).toEqual([
			{ name: 'google / mixed', score: 100 },
			{ name: 'anthropic / fast', score: 60 },
			{ name: 'openai / cheap', score: 29.41 }
		]);
		expect(scoreBars(ranking, 1)).toHaveLength(1);
	});

	it('writes the weights of the preset and switches presets in the query', () => {
		expect(weightsText(ranking)).toBe(
			'Success 30%, first byte p50 15%, first byte p95 15%, output per token 15%, cost 25%'
		);
		expect(weightsText({ ...ranking, kind: 'speech' })).not.toContain('output');
		expect(presetSearch('?organisation_id=org-a&preset=speed', 'cost')).toBe(
			'?organisation_id=org-a&preset=cost'
		);
		expect(presetSearch('', 'cost')).toBe('?preset=cost');
	});
});
