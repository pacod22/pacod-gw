import { describe, expect, it } from 'vitest';
import {
	byProvider,
	reviewItems,
	stepCounts,
	trackedProviders,
	type DiscoveryProposal,
	type PriceProposal,
	type RepoModel
} from './review';

const proposal = (over: Partial<DiscoveryProposal>): DiscoveryProposal => ({
	id: 'dsp_1',
	provider_id: 'prv_ds',
	provider: 'deepseek',
	upstream_id: 'deepseek-flash',
	kind: 'new',
	model_id: null,
	first_seen: '2026-10-30T08:00:00Z',
	...over
});

const model = (over: Partial<RepoModel>): RepoModel => ({
	id: 'mdl_1',
	name: 'deepseek/deepseek-flash',
	provider_id: 'prv_ds',
	provider: 'deepseek',
	upstream_id: 'deepseek-flash',
	state: 'draft',
	priced: false,
	free: false,
	created_at: '2026-10-31T08:00:00Z',
	...over
});

const price = (over: Partial<PriceProposal>): PriceProposal => ({
	id: 'prc_1',
	model_id: 'mdl_1',
	model: 'deepseek/deepseek-flash',
	effective_from: '2026-11-01T03:17:00Z',
	...over
});

describe('reviewItems', () => {
	it('puts each thing at its step of the way a new model goes', () => {
		const items = reviewItems(
			[
				proposal({ id: 'dsp_new' }),
				proposal({ id: 'dsp_gone', kind: 'gone', upstream_id: 'old', model_id: 'mdl_old' })
			],
			[
				price({ id: 'prc_first', model_id: 'mdl_priced' }),
				price({ id: 'prc_change', model_id: 'mdl_old' })
			],
			[
				model({ id: 'mdl_bare', name: 'deepseek/bare' }),
				model({ id: 'mdl_priced', name: 'deepseek/priced' }),
				model({ id: 'mdl_ready', name: 'deepseek/ready', priced: true }),
				model({ id: 'mdl_free', name: 'ollama/free', provider_id: 'prv_ol', free: true }),
				model({ id: 'mdl_old', name: 'deepseek/old', state: 'active', priced: true }),
				model({ id: 'mdl_live', name: 'deepseek/live', state: 'active', priced: true })
			],
			new Set(['prv_ds'])
		);
		const steps = Object.fromEntries(items.map((i) => [i.key, i.step]));
		expect(steps).toEqual({
			'proposal:dsp_new': 'new',
			'proposal:dsp_gone': 'gone',
			// A draft's first price, then a change of a model in use.
			'price:prc_first': 'price_proposed',
			'price:prc_change': 'price_change',
			'model:mdl_bare': 'needs_price',
			'model:mdl_ready': 'ready',
			// A free model needs no price.
			'model:mdl_free': 'ready'
		});
		// A model gone from the provider is named by the repository's name.
		expect(items.find((i) => i.key === 'proposal:dsp_gone')?.name).toBe('deepseek/old');
		// The draft whose price waits is not also listed as without a price.
		expect(items.filter((i) => i.model_id === 'mdl_priced')).toHaveLength(1);
	});

	it('treats a proposal for a draft that has a price as a change, and the draft as ready', () => {
		const items = reviewItems(
			[],
			[price({ id: 'prc_more' })],
			[model({ priced: true })],
			new Set(['prv_ds'])
		);
		expect(items.map((i) => [i.key, i.step])).toEqual([
			['price:prc_more', 'price_change'],
			['model:mdl_1', 'ready']
		]);
	});

	it('says whether the sync of model data looks for a price', () => {
		const models = [
			model({ id: 'mdl_a' }),
			model({ id: 'mdl_b', provider_id: 'prv_other', provider: 'other' })
		];
		const items = reviewItems(
			[],
			[],
			models,
			trackedProviders({ enabled: true, providers: ['prv_ds'] })
		);
		expect(items.map((i) => [i.model_id, i.tracked])).toEqual([
			['mdl_a', true],
			['mdl_b', false]
		]);
		// Switched off, the sync looks for nothing.
		const off = reviewItems(
			[],
			[],
			models,
			trackedProviders({ enabled: false, providers: ['prv_ds'] })
		);
		expect(off.every((i) => !i.tracked)).toBe(true);
	});

	it('names a new model by the provider’s id without Gemini’s prefix', () => {
		const [item] = reviewItems(
			[proposal({ upstream_id: 'models/gemini-3.8-flash', provider: '' })],
			[],
			[],
			new Set()
		);
		expect([item.name, item.provider]).toEqual(['gemini-3.8-flash', 'prv_ds']);
	});
});

describe('byProvider and stepCounts', () => {
	it('groups by provider and orders by step, then by name', () => {
		const items = reviewItems(
			[proposal({ id: 'dsp_b', upstream_id: 'b' }), proposal({ id: 'dsp_a', upstream_id: 'a' })],
			[],
			[
				model({ id: 'mdl_z', name: 'deepseek/z' }),
				model({
					id: 'mdl_o',
					name: 'ollama/x',
					provider_id: 'prv_ol',
					provider: 'Ollama',
					free: true
				})
			],
			new Set()
		);
		const groups = byProvider(items);
		expect(groups.map((g) => [g.provider, g.items.map((i) => i.name)])).toEqual([
			['deepseek', ['a', 'b', 'deepseek/z']],
			['Ollama', ['ollama/x']]
		]);
		expect(stepCounts(items)).toEqual({
			new: 2,
			needs_price: 1,
			price_proposed: 0,
			ready: 1,
			price_change: 0,
			update: 0,
			gone: 0
		});
	});
});

describe('update proposals', () => {
	it('wait as changes at the provider, with what applying them changes', () => {
		const items = reviewItems(
			[
				proposal({
					id: 'dsp_upd',
					kind: 'update',
					upstream_id: 'deepseek-v4-pro',
					model_id: 'mdl_pro',
					details: {
						model: 'deepseek/pro',
						changes: {
							context_window: { current: 128000, reported: 1048576, source: 'provider' },
							'capabilities.json_mode': { current: false, reported: true, source: 'preset' }
						}
					}
				})
			],
			[],
			[model({ id: 'mdl_pro', name: 'deepseek/pro', state: 'active', priced: true })],
			new Set()
		);
		expect(items).toHaveLength(1);
		const [item] = items;
		expect([item.step, item.name, item.proposal_id]).toEqual(['update', 'deepseek/pro', 'dsp_upd']);
		expect(item.changes.map((c) => [c.label, c.source])).toEqual([
			['Context window', 'provider'],
			['JSON mode', 'preset']
		]);
		expect(stepCounts(items).update).toBe(1);
	});
});
