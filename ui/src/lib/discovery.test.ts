import { describe, expect, it } from 'vitest';
import {
	byProvider,
	draftNames,
	matchesQuery,
	nextRun,
	presetsByFamily,
	reportedOf,
	runResults,
	runState,
	takesOf
} from './discovery';

describe('reportedOf', () => {
	it('reads what the provider reported and leaves out the rest', () => {
		const reported = reportedOf('claude-opus-5-5', {
			display_name: 'Claude Opus 5.5',
			context_window: 1_000_000,
			max_output_tokens: 128_000,
			vision: null,
			reasons: true,
			embedding: false
		});
		expect(reported).toEqual({
			display_name: 'Claude Opus 5.5',
			context_window: 1_000_000,
			max_output_tokens: 128_000,
			vision: null,
			reasons: true
		});
		expect(takesOf(reported)).toBe('reasoning');
	});

	it('drops a display name that only repeats the id, with or without the models/ prefix', () => {
		expect(
			reportedOf('models/gemini-3.5-flash', { display_name: 'gemini-3.5-flash' }).display_name
		).toBe(null);
		expect(reportedOf('gpt-4o', { display_name: '  ' }).display_name).toBe(null);
	});

	it('copes with details that are not an object', () => {
		expect(reportedOf('x', null)).toEqual({
			display_name: null,
			context_window: null,
			max_output_tokens: null,
			vision: null,
			reasons: null
		});
		expect(takesOf(reportedOf('x', { vision: true, reasons: true }))).toBe('images, reasoning');
	});
});

describe('draftNames', () => {
	it('names the model as the core does: provider/id, or the id alone', () => {
		expect(draftNames('gemini', 'models/gemini-3.8-flash')).toEqual({
			prefixed: 'gemini/gemini-3.8-flash',
			bare: 'gemini-3.8-flash'
		});
		expect(draftNames('ollama', 'qwen3:latest').prefixed).toBe('ollama/qwen3:latest');
	});
});

describe('byProvider', () => {
	it('groups by provider id, orders providers by name and keeps the order of the rows', () => {
		const rows = [
			{ provider_id: 'p2', provider: 'openai', upstream_id: 'b' },
			{ provider_id: 'p1', provider: 'anthropic', upstream_id: 'x' },
			{ provider_id: 'p2', provider: 'openai', upstream_id: 'a' }
		];
		expect(byProvider(rows).map((g) => [g.provider, g.rows.map((r) => r.upstream_id)])).toEqual([
			['anthropic', ['x']],
			['openai', ['b', 'a']]
		]);
	});
});

describe('matchesQuery', () => {
	const row = {
		upstream_id: 'claude-opus-5-5',
		provider: 'anthropic',
		display_name: 'Claude Opus 5.5'
	};

	it('needs every word in the id, the provider or the name, ignoring case', () => {
		expect(matchesQuery(row, 'OPUS anthropic')).toBe(true);
		expect(matchesQuery(row, 'opus 5.5')).toBe(true);
		expect(matchesQuery(row, 'opus openai')).toBe(false);
		expect(matchesQuery(row, '  ')).toBe(true);
	});
});

describe('runResults', () => {
	it('lists each provider with what it listed or why it could not be asked', () => {
		expect(
			runResults({
				openai: { listed: 4, new: 2, gone: 0, resolved: 0 },
				anthropic: { error: 'the provider answered 401' }
			})
		).toEqual([
			{ provider: 'anthropic', listed: null, error: 'the provider answered 401' },
			{ provider: 'openai', listed: 4, error: null }
		]);
		expect(runResults(null)).toEqual([]);
	});
});

describe('nextRun', () => {
	const now = new Date('2026-10-18T12:00:00Z');

	it('adds the interval to the last start', () => {
		expect(nextRun('2026-10-18T11:00:00Z', 24, now)).toBe('2026-10-19T11:00:00.000Z');
	});

	it('is due when the interval has passed or discovery never ran', () => {
		expect(nextRun('2026-10-17T11:00:00Z', 24, now)).toBe('due');
		expect(nextRun(undefined, 24, now)).toBe('due');
	});

	it('is none when discovery runs only when asked', () => {
		expect(nextRun('2026-10-18T11:00:00Z', 0, now)).toBe(null);
	});
});

describe('presetsByFamily', () => {
	it('groups presets by family, families in order, presets as they came', () => {
		const presets = [
			{ name: 'openai-chat', family: 'openai' },
			{ name: 'anthropic-adaptive', family: 'anthropic' },
			{ name: 'embedding', family: 'openai' }
		];
		expect(presetsByFamily(presets).map((g) => [g.family, g.presets.map((p) => p.name)])).toEqual([
			['anthropic', ['anthropic-adaptive']],
			['openai', ['openai-chat', 'embedding']]
		]);
	});
});

describe('runState', () => {
	it('is running while the run is open, however long it takes', () => {
		expect(runState({ finished_at: null })).toBe('running');
		expect(runState({})).toBe('running');
	});

	it('tells a failed run from a finished one', () => {
		const ended = { finished_at: '2026-10-18T11:01:00Z' };
		expect(runState({ ...ended, ok: false })).toBe('failed');
		expect(runState({ ...ended, ok: true })).toBe('finished');
	});
});
