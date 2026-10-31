/**
 * Templates: the parts of the editor that are not a plain form. A parameter of a template
 * has three states, which no generic control expresses: not set (the caller decides), never
 * sent (even when the caller sends it), or a value, which can be locked against the caller.
 */
import { readForm, type Field } from './schema/form';

export interface ParamValue<T> {
	value: T | null;
	/** The caller cannot override the value. */
	locked: boolean;
	/** Never send this parameter, even when the caller does. */
	omit: boolean;
}

export type ParamState = 'unset' | 'omit' | 'value';

export type ParamType = 'number' | 'integer' | 'text' | 'boolean';

export interface ParamDefinition {
	/** The name in the form and, dots as nesting, in the spec: `reasoning.effort`. */
	name: string;
	label: string;
	help: string;
	type: ParamType;
	min?: number;
	max?: number;
	step?: number;
	/** The capability of the model that says whether it accepts the parameter. */
	capability: 'temperature' | 'top_p' | 'penalties' | 'reasoning';
}

export const SAMPLING: readonly ParamDefinition[] = [
	{
		name: 'temperature',
		label: 'Temperature',
		help: 'Randomness of the answer, usually 0 to 2.',
		type: 'number',
		min: 0,
		max: 2,
		step: 0.05,
		capability: 'temperature'
	},
	{
		name: 'top_p',
		label: 'Top p',
		help: 'Nucleus sampling: the share of probability mass considered, 0 to 1.',
		type: 'number',
		min: 0,
		max: 1,
		step: 0.01,
		capability: 'top_p'
	},
	{
		name: 'frequency_penalty',
		label: 'Frequency penalty',
		help: 'Discourages repeating the same words, usually -2 to 2.',
		type: 'number',
		min: -2,
		max: 2,
		step: 0.1,
		capability: 'penalties'
	},
	{
		name: 'presence_penalty',
		label: 'Presence penalty',
		help: 'Encourages new topics, usually -2 to 2.',
		type: 'number',
		min: -2,
		max: 2,
		step: 0.1,
		capability: 'penalties'
	}
];

export const REASONING: readonly ParamDefinition[] = [
	{
		name: 'reasoning.effort',
		label: 'Reasoning effort',
		help: 'The effort level, for example low or high, or none to switch reasoning off.',
		type: 'text',
		capability: 'reasoning'
	},
	{
		name: 'reasoning.budget_tokens',
		label: 'Reasoning budget',
		help: 'Budget-style models: the thinking budget in tokens.',
		type: 'integer',
		min: 0,
		step: 1,
		capability: 'reasoning'
	},
	{
		name: 'reasoning.summary',
		label: 'Reasoning summary',
		help: 'Return a summary of the reasoning.',
		type: 'boolean',
		capability: 'reasoning'
	}
];

/** The paths of the spec that the editor renders itself; the generic form leaves them out. */
export const HAND_BUILT = [
	'spec.model',
	'spec.models',
	'spec.routing_scheme',
	'spec.temperature',
	'spec.top_p',
	'spec.frequency_penalty',
	'spec.presence_penalty',
	'spec.reasoning',
	'spec.tags'
];

function isRecord(value: unknown): value is Record<string, unknown> {
	return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** The parameter at a dotted name inside a spec. */
export function paramOf(spec: unknown, name: string): ParamValue<unknown> | null {
	let current: unknown = spec;
	for (const part of name.split('.')) {
		if (!isRecord(current)) return null;
		current = current[part];
	}
	return isRecord(current) ? (current as unknown as ParamValue<unknown>) : null;
}

export function paramState(param: ParamValue<unknown> | null | undefined): ParamState {
	if (!param) return 'unset';
	return param.omit ? 'omit' : 'value';
}

/** A parameter in words: "0.2, locked", "never sent", "not set". */
export function describeParam(param: ParamValue<unknown> | null | undefined): string {
	if (!param) return 'Not set: the caller decides';
	if (param.omit) return 'Never sent, even when the caller sends it';
	const value =
		typeof param.value === 'boolean' ? (param.value ? 'yes' : 'no') : String(param.value);
	return param.locked
		? `${value}, locked against the caller`
		: `${value}, unless the caller sends another`;
}

/**
 * What a model's capability profile says about a parameter, when it is not simply accepted.
 * Empty when there is nothing to warn about.
 */
export function capabilityNote(
	definition: ParamDefinition,
	capabilities: Record<string, unknown> | undefined
): string {
	if (!capabilities) return '';
	const support = capabilities[definition.capability];
	if (definition.capability === 'reasoning') {
		if (support === 'none')
			return 'This model’s reasoning cannot be steered: a value here is stored but never sent.';
		if (definition.name === 'reasoning.budget_tokens' && support !== 'budget') {
			return 'This model takes an effort level, not a budget.';
		}
		if (definition.name === 'reasoning.effort' && support === 'budget') {
			return 'This model takes a budget, not an effort level.';
		}
		return '';
	}
	if (support === 'rejected')
		return 'This model rejects the parameter: a value here is stored but never sent.';
	if (support === 'default_only')
		return 'This model accepts the parameter only at its default value.';
	return '';
}

type FormSource = Pick<FormData, 'get' | 'getAll' | 'has'>;

function text(form: FormSource, name: string): string {
	const value = form.get(name);
	return typeof value === 'string' ? value : '';
}

function parse(type: ParamType, raw: string): unknown {
	const trimmed = raw.trim();
	if (type === 'boolean') return trimmed === 'true';
	if (type === 'text') return trimmed === '' ? null : trimmed;
	if (trimmed === '') return null;
	const number = Number(trimmed);
	// Not a number: pass it on as typed so that the API reports the problem.
	return Number.isFinite(number) ? number : trimmed;
}

/** Reads one parameter of the editor: `p.<name>.state`, `.value`, `.locked`. */
export function readParam(
	form: FormSource,
	definition: ParamDefinition
): ParamValue<unknown> | null {
	const state = text(form, `p.${definition.name}.state`);
	if (state === 'omit') return { value: null, locked: false, omit: true };
	if (state !== 'value') return null;
	return {
		value: parse(definition.type, text(form, `p.${definition.name}.value`)),
		locked: text(form, `p.${definition.name}.locked`) === 'on',
		omit: false
	};
}

/** One model of a template's pool. */
export interface PoolEntry {
	model: string;
	tier?: string | null;
	weight?: number;
	priority?: number;
}

/** The models of a template: its pool, or its single model as a pool of one. */
export function poolOf(
	spec: { model?: string; models?: PoolEntry[] } | null | undefined
): PoolEntry[] {
	if (spec?.models && spec.models.length > 0) return spec.models;
	return spec?.model ? [{ model: spec.model, tier: null, weight: 1, priority: 0 }] : [];
}

/**
 * The tiers routing schemes choose, once each and sorted: the values a pool's tier can
 * usefully take, since a tier no scheme names is never chosen.
 */
export function schemeTiers(
	schemes: { rules?: { then: { tiers?: string[] } }[]; default?: { tiers?: string[] } }[]
): string[] {
	const all = schemes.flatMap((s) => [
		...(s.rules ?? []).flatMap((r) => r.then.tiers ?? []),
		...(s.default?.tiers ?? [])
	]);
	return [...new Set(all)].sort();
}

/** The models of a template in a few words: its model, or "3 models: a, b, c". */
export function describeModels(spec: { model?: string; models?: PoolEntry[] }): string {
	if (!spec.models || spec.models.length === 0) return spec.model ?? '';
	return `${spec.models.length} models: ${spec.models.map((m) => m.model).join(', ')}`;
}

/** The pool rows of the editor: `pool.model`, `pool.tier`, `pool.weight`, `pool.priority`. */
export function readPool(form: FormSource): PoolEntry[] {
	const all = (name: string) =>
		form.getAll(name).map((v) => (typeof v === 'string' ? v.trim() : ''));
	const models = all('pool.model');
	const tiers = all('pool.tier');
	const weights = all('pool.weight');
	const priorities = all('pool.priority');
	const number = (raw: string | undefined, fallback: number) => {
		if (!raw) return fallback;
		const n = Number(raw);
		return Number.isFinite(n) ? n : fallback;
	};
	return models
		.map((model, i) => ({
			model,
			tier: tiers[i] ? tiers[i] : null,
			weight: number(weights[i], 1),
			priority: number(priorities[i], 0)
		}))
		.filter((e) => e.model !== '');
}

/**
 * The request body of a template from the editor's form: the plain fields through the
 * generic reader, the model, the parameters and the tags from the editor's own controls.
 *
 * @param fields the fields of `TemplateInput` without the hand-built ones
 * @param tagKeys the tag keys the workspace declares; other tags are not sent
 */
export function readTemplateForm(
	form: FormSource,
	fields: Field[],
	tagKeys: readonly string[]
): Record<string, unknown> {
	const body = readForm(fields, form);
	const plain = isRecord(body.spec) ? body.spec : {};
	const param = (name: string, list: readonly ParamDefinition[]) => {
		const definition = list.find((d) => d.name === name);
		return definition ? readParam(form, definition) : null;
	};
	const tags: Record<string, string> = {};
	for (const key of tagKeys) {
		const value = text(form, `tag.${key}`).trim();
		if (value) tags[key] = value;
	}
	const pooled = text(form, 'model_mode') === 'pool';
	return {
		...body,
		spec: {
			...plain,
			model: pooled ? '' : text(form, 'model'),
			models: pooled ? readPool(form) : [],
			routing_scheme: pooled ? text(form, 'routing_scheme') || null : null,
			temperature: param('temperature', SAMPLING),
			top_p: param('top_p', SAMPLING),
			frequency_penalty: param('frequency_penalty', SAMPLING),
			presence_penalty: param('presence_penalty', SAMPLING),
			reasoning: {
				effort: param('reasoning.effort', REASONING),
				budget_tokens: param('reasoning.budget_tokens', REASONING),
				summary: param('reasoning.summary', REASONING)
			},
			tags
		}
	};
}

/** What a check of a template found, as sentences. Empty when all is well. */
export function checkFindings(check: {
	ignored_params: string[];
	model_state: string;
	successor?: string | null;
	models?: { model: string; state: string; successor?: string | null; ignored_params: string[] }[];
	routing?: { warnings: string[] };
}): string[] {
	const out: string[] = [];
	const models =
		check.models && check.models.length > 0
			? check.models
			: [
					{
						model: '',
						state: check.model_state,
						successor: check.successor,
						ignored_params: check.ignored_params
					}
				];
	const pooled = models.length > 1;
	for (const m of models) {
		// "The model" for a template with one; the model's name in a pool.
		const the = pooled ? `The model ${m.model}` : 'The model';
		const of = pooled ? `The model ${m.model} of this template` : 'The model of this template';
		if (m.ignored_params.length > 0) {
			out.push(
				`${the} does not accept ${m.ignored_params.join(', ')}. The template's values for ${m.ignored_params.length === 1 ? 'it' : 'them'} are stored but never sent${pooled ? ' to it' : ''}.`
			);
		}
		const successor = m.successor ? ` Its successor is ${m.successor}.` : ' It has no successor.';
		if (m.state === 'deprecated') out.push(`${of} is deprecated.${successor}`);
		if (m.state === 'retired') {
			out.push(
				m.successor
					? `${of} is retired. Calls are served by its successor ${m.successor} where the grant allows it.`
					: pooled
						? `${of} is retired and has no successor: calls go to the other models.`
						: 'The model of this template is retired and has no successor: calls with this template fail.'
			);
		}
		if (m.state === 'disabled')
			out.push(
				pooled
					? `${of} is disabled: calls go to the other models until it is enabled again.`
					: 'The model of this template is disabled: calls with this template fail until it is enabled again.'
			);
		if (m.state === 'draft') out.push(`${of} is still a draft and cannot be called yet.`);
	}
	out.push(
		...(check.routing?.warnings ?? []).map((w) => w.charAt(0).toUpperCase() + w.slice(1) + '.')
	);
	return out;
}

/** What a template's test call answered, for the template page to show. */
export interface TestCall {
	text: string;
	model: string;
	cost: string;
	latency_ms: number | null;
	request_id: string;
}
