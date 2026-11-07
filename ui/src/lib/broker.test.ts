import { describe, expect, it } from 'vitest';
import {
	changeBody,
	changeSummary,
	choicesFrom,
	editable,
	modelBlocked,
	offerChanges,
	templateBlocked,
	type ShownRow
} from './broker';

describe('what can be offered', () => {
	it('needs a broker key, a workspace that may call the model, and a model in service', () => {
		const m = { key: 'model:openai/gpt-4o', reached_by: ['ws_1'], state: 'active' as const };
		expect(modelBlocked(m)).toBeNull();
		expect(modelBlocked({ ...m, state: 'deprecated' })).toBeNull();
		expect(modelBlocked({ ...m, key: null })).toMatch(/not a broker key/);
		expect(modelBlocked({ ...m, reached_by: [] })).toMatch(/no workspace/);
		expect(modelBlocked({ ...m, state: 'retired' })).toBe('it is retired');
	});

	it('needs an active template that a workspace reaches', () => {
		expect(templateBlocked({ status: 'active', reached_by: ['ws_1'] })).toBeNull();
		expect(templateBlocked({ status: 'disabled', reached_by: ['ws_1'] })).toBe('it is disabled');
		expect(templateBlocked({ status: 'active', reached_by: [] })).toMatch(/of its own/);
	});

	it('keeps a blocked row that is offered, so that it can be withdrawn', () => {
		const rows = [
			{ id: 'a', blocked: false, offer: null },
			{ id: 'b', blocked: true, offer: null },
			{ id: 'c', blocked: true, offer: { visibility: 'platform' } }
		];
		const shown = editable(
			rows,
			(r) => r.id,
			(r) => (r.blocked ? 'no' : null)
		);
		expect(shown.map((r) => r.id)).toEqual(['a', 'c']);
	});
});

describe('the change the page posts', () => {
	const shown: ShownRow[] = [
		{ id: 'tpl_1', offer: null },
		{ id: 'tpl_2', offer: { visibility: 'platform' } },
		{ id: 'tpl_3', offer: { visibility: 'public', summary: 'Contracts' } },
		{ id: 'tpl_4', offer: null }
	];

	function form(ticked: string[], fields: Record<string, string> = {}): FormData {
		const f = new FormData();
		for (const id of ticked) f.append('template', id);
		for (const [k, v] of Object.entries(fields)) f.set(k, v);
		return f;
	}

	it('sends only the rows whose offer the form changes', () => {
		const chosen = choicesFrom(
			form(['tpl_1', 'tpl_3'], {
				'visibility:tpl_1': 'public',
				'visibility:tpl_3': 'public',
				'summary:tpl_3': ' Contracts '
			}),
			'template',
			shown
		);
		const changes = offerChanges(shown, chosen);
		expect(changes).toEqual([
			{ id: 'tpl_1', offer: { visibility: 'public' } },
			{ id: 'tpl_2', offer: null }
		]);
		expect(changeSummary(shown, changes)).toBe('1 offered, 1 withdrawn');
	});

	it('takes the organisation as the default visibility, and a new summary as a change', () => {
		const chosen = choicesFrom(
			form(['tpl_2', 'tpl_3', 'tpl_4'], {
				'visibility:tpl_2': 'platform',
				'visibility:tpl_3': 'public',
				'summary:tpl_3': 'Contracts in German'
			}),
			'template',
			shown
		);
		const changes = offerChanges(shown, chosen);
		expect(changes).toEqual([
			{ id: 'tpl_3', offer: { visibility: 'public', summary: 'Contracts in German' } },
			{ id: 'tpl_4', offer: { visibility: 'organisation' } }
		]);
		expect(changeSummary(shown, changes)).toBe('1 offered, 1 changed');
	});

	it('ignores ids the page did not show', () => {
		const chosen = choicesFrom(form(['tpl_9']), 'template', shown);
		expect(offerChanges(shown, chosen).map((c) => c.id)).toEqual(['tpl_2', 'tpl_3']);
	});

	it('names models by model_id in the body', () => {
		expect(
			changeBody([{ id: 'mdl_1', offer: null }], [{ id: 'tpl_1', offer: { visibility: 'public' } }])
		).toEqual({
			models: [{ model_id: 'mdl_1', offer: null }],
			templates: [{ id: 'tpl_1', offer: { visibility: 'public' } }]
		});
	});
});
