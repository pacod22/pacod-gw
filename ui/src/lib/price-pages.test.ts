import { describe, expect, it } from 'vitest';
import type { Schemas } from '#lib/api/types.ts';
import { pageGroups, pageNote } from './price-pages';

type Page = Schemas['Item16'];

const page = (over: Partial<Page>): Page =>
	({
		id: 'ppg_1',
		version: 1,
		created_at: '2026-11-05T00:00:00Z',
		updated_at: '2026-11-05T00:00:00Z',
		provider_id: 'prv_ds',
		provider: 'deepseek',
		url: 'https://api-docs.deepseek.com/quick_start/pricing',
		kind: 'page',
		format: 'pacod',
		status: 'active',
		found_by: 'preset',
		priority: 0,
		...over
	}) as Page;

describe('pageGroups', () => {
	it('groups the pages by provider, providers by name', () => {
		const groups = pageGroups([
			page({ id: 'a', provider_id: 'prv_z', provider: 'zai' }),
			page({ id: 'b' }),
			page({ id: 'c', provider_id: 'prv_z', provider: 'zai' }),
			page({ id: 'd', provider_id: 'prv_gone', provider: '' })
		]);
		expect(groups.map((g) => [g.provider, g.pages.map((p) => p.id)])).toEqual([
			['deepseek', ['b']],
			['prv_gone', ['d']],
			['zai', ['a', 'c']]
		]);
	});
});

describe('pageNote', () => {
	it('says what the last check and the scan found', () => {
		expect(pageNote(page({}))).toBe('');
		const state = {
			outcome: 'failed',
			message: 'the page answered 403',
			consecutive_failures: 3,
			check_requested: true,
			flag: 'moved',
			moved_to: 'https://x.example/new'
		} as Page['state'];
		expect(pageNote(page({ state }))).toBe(
			'moved to https://x.example/new; failed: the page answered 403; 3 failures in a row; to be checked at the next run'
		);
		expect(
			pageNote(page({ status: 'proposed', score: 24, moved_from: 'https://x.example/old' }))
		).toBe('scored 24; replaces https://x.example/old');
	});
});
