import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { limitAmount, quantity, usedShare } from './limits';
import { stateChanges } from './models';
import { otherRates, rate } from './prices';
import { buildFields } from './schema/form';
import { flattenValues } from './schema/fill';
import type { OpenApiDoc } from './schema/spec';
import {
	capabilityNote,
	checkFindings,
	describeModels,
	poolOf,
	type PoolEntry,
	describeParam,
	HAND_BUILT,
	paramOf,
	paramState,
	readTemplateForm,
	REASONING,
	SAMPLING,
	schemeTiers
} from './templates';

const doc = JSON.parse(
	readFileSync(new URL('./api/openapi.json', import.meta.url), 'utf8')
) as OpenApiDoc;
const fields = buildFields(
	doc,
	{ $ref: '#/components/schemas/TemplateInput' },
	{
		overrides: Object.fromEntries(HAND_BUILT.map((path) => [path, { hidden: true }]))
	}
);

function form(entries: Record<string, string>): FormData {
	const data = new FormData();
	for (const [name, value] of Object.entries(entries)) data.append(name, value);
	return data;
}

describe('the template editor', () => {
	it('covers every parameter of the spec that has the three states', () => {
		const spec = doc.components?.schemas?.TemplateSpec;
		const properties = typeof spec === 'object' ? (spec.properties ?? {}) : {};
		const params = Object.entries(properties)
			.filter(([, schema]) => JSON.stringify(schema).includes('/Param'))
			.map(([key]) => key);
		expect(params.sort()).toEqual(SAMPLING.map((d) => d.name).sort());
		const reasoning = doc.components?.schemas?.ReasoningSpec;
		expect(Object.keys(typeof reasoning === 'object' ? (reasoning.properties ?? {}) : {})).toEqual(
			REASONING.map((d) => d.name.replace('reasoning.', ''))
		);
	});

	it('leaves the plain fields to the schema', () => {
		const paths = fields.flatMap((f) => [f.path, ...(f.fields ?? []).map((c) => c.path)]);
		expect(paths).toEqual(
			expect.arrayContaining([
				'name',
				'description',
				'comment',
				'status',
				'spec.prompt_cache',
				'spec.audit',
				'spec.proxy'
			])
		);
		for (const path of HAND_BUILT) expect(paths).not.toContain(path);
	});

	it('reads the three states of a parameter', () => {
		const body = readTemplateForm(
			form({
				name: 'planner',
				description: 'Plans',
				comment: 'first',
				status: 'active',
				model: 'mock/gpt-test',
				'p.temperature.state': 'value',
				'p.temperature.value': '0.2',
				'p.temperature.locked': 'on',
				'p.top_p.state': 'omit',
				'p.frequency_penalty.state': 'unset',
				'p.reasoning.effort.state': 'value',
				'p.reasoning.effort.value': 'low',
				'p.reasoning.budget_tokens.state': 'value',
				'p.reasoning.budget_tokens.value': '2048',
				'p.reasoning.summary.state': 'value',
				'p.reasoning.summary.value': 'true',
				'spec.max_output_tokens': '1000',
				'spec.timeout_seconds': '',
				'spec.prompt_cache': 'auto',
				'spec.unsupported_params': '',
				'spec.audit': 'inherit',
				'spec.proxy': 'check',
				'spec.request_stream_usage': 'on',
				'tag.purpose': 'planner',
				'tag.site': '  ',
				'tag.undeclared': 'x'
			}),
			fields,
			['purpose', 'site']
		);
		expect(body).toMatchObject({
			name: 'planner',
			description: 'Plans',
			comment: 'first',
			status: 'active'
		});
		expect(body.spec).toEqual({
			model: 'mock/gpt-test',
			models: [],
			routing_scheme: null,
			max_output_tokens: 1000,
			timeout_seconds: null,
			retry_max_attempts: null,
			prompt_cache: 'auto',
			unsupported_params: null,
			audit: 'inherit',
			proxy: 'check',
			request_stream_usage: true,
			temperature: { value: 0.2, locked: true, omit: false },
			top_p: { value: null, locked: false, omit: true },
			frequency_penalty: null,
			presence_penalty: null,
			reasoning: {
				effort: { value: 'low', locked: false, omit: false },
				budget_tokens: { value: 2048, locked: false, omit: false },
				summary: { value: true, locked: false, omit: false }
			},
			// Only declared tag keys with a value are sent.
			tags: { purpose: 'planner' }
		});
	});

	it('reads a pool of models and its routing scheme', () => {
		const data = new FormData();
		data.append('name', 'bot');
		data.append('model_mode', 'pool');
		data.append('model', 'ignored/single');
		data.append('routing_scheme', 'tiers');
		for (const [model, tier, weight, priority] of [
			['mock/small', 'small', '3', ''],
			['', 'x', '1', '0'],
			['mock/large', '', '', '1']
		]) {
			data.append('pool.model', model);
			data.append('pool.tier', tier);
			data.append('pool.weight', weight);
			data.append('pool.priority', priority);
		}
		const spec = readTemplateForm(data, [], []).spec as Record<string, unknown>;
		expect(spec.model).toBe('');
		expect(spec.routing_scheme).toBe('tiers');
		expect(spec.models).toEqual([
			{ model: 'mock/small', tier: 'small', weight: 3, priority: 0 },
			{ model: 'mock/large', tier: null, weight: 1, priority: 1 }
		]);
		expect(poolOf({ model: 'a/b' })).toEqual([
			{ model: 'a/b', tier: null, weight: 1, priority: 0 }
		]);
		expect(describeModels({ model: '', models: spec.models as PoolEntry[] })).toBe(
			'2 models: mock/small, mock/large'
		);
	});

	it('offers the tiers routing schemes choose, once each', () => {
		const rule = (tiers: string[]) => ({ then: { tiers } });
		expect(
			schemeTiers([
				{ rules: [rule(['small']), rule(['large', 'small'])], default: {} },
				{ rules: [], default: { tiers: ['medium'] } },
				{}
			])
		).toEqual(['large', 'medium', 'small']);
	});

	it('describes a stored parameter', () => {
		const spec = {
			temperature: { value: 0.2, locked: true, omit: false },
			top_p: { value: null, locked: false, omit: true },
			reasoning: { effort: null }
		};
		expect(paramState(paramOf(spec, 'temperature'))).toBe('value');
		expect(paramState(paramOf(spec, 'top_p'))).toBe('omit');
		expect(paramState(paramOf(spec, 'reasoning.effort'))).toBe('unset');
		expect(describeParam(paramOf(spec, 'temperature'))).toBe('0.2, locked against the caller');
		expect(describeParam(paramOf(spec, 'top_p'))).toMatch(/Never sent/);
		expect(describeParam(null)).toMatch(/Not set/);
	});

	it('warns where the model does not accept a parameter', () => {
		const temperature = SAMPLING[0];
		expect(capabilityNote(temperature, { temperature: 'accepted' })).toBe('');
		expect(capabilityNote(temperature, { temperature: 'rejected' })).toMatch(/never sent/);
		expect(capabilityNote(temperature, { temperature: 'default_only' })).toMatch(/default value/);
		expect(capabilityNote(REASONING[0], { reasoning: 'none' })).toMatch(/cannot be steered/);
		expect(capabilityNote(REASONING[1], { reasoning: 'effort' })).toMatch(/not a budget/);
		expect(capabilityNote(REASONING[0], { reasoning: 'adaptive' })).toBe('');
		expect(capabilityNote(temperature, undefined)).toBe('');
	});

	it('turns the check of a template into sentences', () => {
		expect(checkFindings({ ignored_params: [], model_state: 'active', successor: null })).toEqual(
			[]
		);
		const findings = checkFindings({
			ignored_params: ['temperature', 'top_p'],
			model_state: 'deprecated',
			successor: 'mock/next'
		});
		expect(findings[0]).toMatch(/does not accept temperature, top_p/);
		expect(findings[1]).toBe(
			'The model of this template is deprecated. Its successor is mock/next.'
		);
		expect(
			checkFindings({ ignored_params: [], model_state: 'retired', successor: null })[0]
		).toMatch(/calls with this template fail/);

		// A pool names each model, and a model that is out leaves the others.
		const pool = checkFindings({
			ignored_params: [],
			model_state: 'active',
			models: [
				{ model: 'a/one', state: 'active', ignored_params: ['top_p'] },
				{ model: 'a/two', state: 'disabled', ignored_params: [] }
			],
			routing: {
				warnings: [
					'rule "gpu" of the routing scheme "tiers" names the tiers gpu this pool does not have; the rule never applies'
				]
			}
		});
		expect(pool).toEqual([
			"The model a/one does not accept top_p. The template's values for it are stored but never sent to it.",
			'The model a/two of this template is disabled: calls go to the other models until it is enabled again.',
			'Rule "gpu" of the routing scheme "tiers" names the tiers gpu this pool does not have; the rule never applies.'
		]);
	});
});

describe('the life cycle of a model', () => {
	it('offers the changes the core accepts', () => {
		const to = (state: string, previous?: string | null) =>
			stateChanges(state, previous).map((c) => c.to);
		expect(to('draft')).toEqual(['active']);
		expect(to('active')).toEqual(['deprecated', 'retired', 'disabled']);
		expect(to('deprecated')).toEqual(['retired', 'active', 'disabled']);
		expect(to('retired')).toEqual(['active', 'deprecated']);
		expect(to('disabled', 'deprecated')).toEqual(['deprecated']);
		expect(to('disabled', null)).toEqual(['active']);
		expect(to('unknown')).toEqual([]);
	});
});

describe('wording of limits and prices', () => {
	it('words an allowance and its use', () => {
		expect(limitAmount({ amount: 100000, metric: 'tokens', window: 'day' })).toBe(
			'100,000 tokens per day'
		);
		expect(limitAmount({ amount: 25.5, metric: 'spend', window: 'lifetime' })).toBe(
			'25.5 currency units in total'
		);
		expect(quantity(3, 'requests')).toBe('3 requests');
		expect(usedShare(50, 200)).toBe(0.25);
		expect(usedShare(300, 200)).toBe(1.5);
		// Nothing is allowed: whatever was used, the limit is used up.
		expect(usedShare(0, 0)).toBe(1);
	});

	it('writes rates and leaves out what is not billed', () => {
		expect(rate(3)).toBe('3.00');
		expect(rate(0.075)).toBe('0.075');
		expect(rate(null)).toBe('');
		expect(otherRates({ per_request: 0.01, variants: { batch: {} } })).toBe(
			'0.01 per request; variants: batch'
		);
		expect(otherRates({})).toBe('');
	});
});

describe('filling a form from a preset', () => {
	it('names every value by its field path', () => {
		const values = flattenValues({
			name: 'openai',
			quirks: { stream_usage: true, drop_fields: ['a', 'b'] }
		});
		expect([...values]).toEqual([
			['name', 'openai'],
			['quirks.stream_usage', true],
			['quirks.drop_fields', ['a', 'b']]
		]);
		expect([...flattenValues({ temperature: 'rejected' }, 'capabilities')]).toEqual([
			['capabilities.temperature', 'rejected']
		]);
	});
});
