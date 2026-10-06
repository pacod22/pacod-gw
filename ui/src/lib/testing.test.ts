import { describe, expect, it } from 'vitest';
import {
	countsText,
	formatDuration,
	isFinished,
	runDuration,
	statusTone,
	unavailable,
	type CatalogCase
} from './testing';

const testCase = (more: Partial<CatalogCase> = {}): CatalogCase => ({
	id: 'c',
	title: 'c',
	description: '',
	modes: ['mock', 'live'],
	needs: [],
	...more
});

const everything = { openai: true, anthropic: true, mockControl: true };

describe('unavailable', () => {
	it('lets a case run when its mode and needs are met', () => {
		expect(unavailable(testCase({ needs: ['openai'] }), 'live', everything)).toBeNull();
	});

	it('names the mode a case is limited to', () => {
		expect(unavailable(testCase({ modes: ['mock'] }), 'live', everything)).toBe(
			'runs in mock mode only'
		);
	});

	it('names what is missing', () => {
		const have = { openai: false, anthropic: true, mockControl: false };
		expect(unavailable(testCase({ needs: ['openai', 'mock_control'] }), 'mock', have)).toBe(
			'needs an OpenAI-dialect model and a mock provider it can script'
		);
	});
});

describe('formatting', () => {
	it('writes durations for people', () => {
		expect(formatDuration(null)).toBe('–');
		expect(formatDuration(240.4)).toBe('240 ms');
		expect(formatDuration(1520)).toBe('1.5 s');
		expect(formatDuration(125_000)).toBe('2 min 5 s');
	});

	it('measures runs, also while they go', () => {
		const started = '2026-10-06T10:00:00Z';
		expect(runDuration({ started_at: null, finished_at: null })).toBeNull();
		expect(runDuration({ started_at: started, finished_at: '2026-10-06T10:00:03Z' })).toBe(3000);
		expect(runDuration({ started_at: started, finished_at: null }, Date.parse(started) + 500)).toBe(
			500
		);
	});

	it('lists only the counts that are not zero', () => {
		expect(countsText({ total: 4, passed: 3, failed: 1, error: 0, skipped: 0 })).toBe(
			'3 passed, 1 failed'
		);
		expect(countsText({ total: 2, passed: 0, failed: 0, error: 0, skipped: 0 })).toBe('2 cases');
		expect(countsText({ total: 3, passed: 0, failed: 0, error: 1, skipped: 2 })).toBe(
			'1 error, 2 skipped'
		);
	});

	it('gives statuses a tone', () => {
		expect(statusTone('passed')).toBe('good');
		expect(statusTone('error')).toBe('bad');
		expect(statusTone('running')).toBe('warn');
		expect(statusTone('skipped')).toBe('neutral');
		expect(isFinished('running')).toBe(false);
		expect(isFinished('cancelled')).toBe(true);
	});
});
