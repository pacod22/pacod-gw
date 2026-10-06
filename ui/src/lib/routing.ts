/**
 * Routing schemes in the console: the scheme as the editor holds it (plain strings for text
 * inputs), and back. A condition on a text value of a call is written in one line:
 *
 * - empty: any value, including none
 * - `*`: a value must be given
 * - `-`: no value may be given
 * - otherwise a list separated by commas: `review` equals, `ci-*` starts with
 */
import type { Schemas } from '#lib/api/types.ts';

export type RoutingScheme = Schemas['RoutingScheme'];
export type RouteRule = Schemas['RouteRule'];
export type RouteAction = Schemas['RouteAction'];
export type FailoverPolicy = Schemas['FailoverPolicy'];
export type StrMatch = Schemas['StrMatch'];
export type Algorithm = Schemas['Algorithm'];
export type ComplexityMatch = Schemas['ComplexityMatch'];

export const ALGORITHMS: { value: Algorithm; label: string; help: string }[] = [
	{
		value: 'priority',
		label: 'Main and backups',
		help: 'The lowest priority serves; the next take over when it fails.'
	},
	{ value: 'weighted', label: 'Weighted', help: 'A random pick in proportion to the weights.' },
	{ value: 'round_robin', label: 'Round robin', help: 'Each model in turn.' },
	{
		value: 'weighted_round_robin',
		label: 'Weighted round robin',
		help: 'In turn, as often as the weights say, evenly interleaved.'
	},
	{
		value: 'least_in_flight',
		label: 'Least busy',
		help: 'Of two weighted picks, the one with fewer calls in progress.'
	},
	{
		value: 'lowest_latency',
		label: 'Fastest',
		help: 'The lowest time to the first byte this gateway measured; now and then another, to keep measuring.'
	},
	{ value: 'cheapest', label: 'Cheapest', help: 'The lowest price per token.' }
];

export const COMPLEXITY_MATCHES: ComplexityMatch[] = ['low', 'medium', 'high', 'unset'];

export interface FailoverDraft {
	max_targets: number;
	attempts_per_target: string;
	per_target_timeout_ms: string;
	deadline_ms: string;
	rate_limited: boolean;
	server_error: boolean;
	connect_error: boolean;
	limit_refused: boolean;
	timeout: boolean;
}

export interface ActionDraft {
	/** Tiers separated by commas. */
	tiers: string;
	algorithm: Algorithm;
	affinity: 'none' | 'session';
	overflow: 'none' | 'rest_of_pool';
	/** A failover policy of its own; unset takes the scheme's. */
	failover: FailoverDraft | null;
}

export interface RuleDraft {
	name: string;
	complexity: ComplexityMatch[];
	agent_id: string;
	task: string;
	user_id: string;
	template: string;
	/** One tag condition per line: `key = condition`. */
	tags: string;
	at_least: string;
	below: string;
	cel: string;
	then: ActionDraft;
}

export interface SchemeDraft {
	name: string;
	description: string;
	status: 'active' | 'disabled';
	rules: RuleDraft[];
	default: ActionDraft;
	failover: FailoverDraft;
}

const split = (text: string) =>
	text
		.split(',')
		.map((s) => s.trim())
		.filter((s) => s !== '');

/** A condition from its line. */
export function parseMatch(text: string): StrMatch {
	const t = text.trim();
	if (t === '') return {};
	if (t === '*') return { present: true };
	if (t === '-') return { present: false };
	const items = split(t);
	const out: StrMatch = {};
	const exact = items.filter((i) => !i.endsWith('*'));
	const prefix = items
		.filter((i) => i.endsWith('*'))
		.map((i) => i.slice(0, -1))
		.filter((i) => i !== '');
	if (exact.length > 0) out.in = exact;
	if (prefix.length > 0) out.prefix = prefix;
	return out;
}

/** A condition as its line. */
export function formatMatch(m: StrMatch | undefined): string {
	if (!m) return '';
	const items = [...(m.in ?? []), ...(m.prefix ?? []).map((p) => `${p}*`)];
	if (items.length > 0) return items.join(', ');
	if (m.present === true) return '*';
	if (m.present === false) return '-';
	return '';
}

function parseTags(text: string): Record<string, StrMatch> {
	const out: Record<string, StrMatch> = {};
	for (const line of text.split('\n')) {
		const at = line.indexOf('=');
		if (at < 0) continue;
		const key = line.slice(0, at).trim();
		if (key) out[key] = parseMatch(line.slice(at + 1));
	}
	return out;
}

function formatTags(tags: Record<string, StrMatch> | undefined): string {
	return Object.entries(tags ?? {})
		.map(([k, m]) => `${k} = ${formatMatch(m)}`)
		.join('\n');
}

const numberOrNull = (text: string): number | null => {
	const t = text.trim();
	if (t === '') return null;
	const n = Number(t);
	return Number.isFinite(n) ? n : null;
};

const text = (n: number | null | undefined) => (n === null || n === undefined ? '' : String(n));

export function failoverDraft(f?: FailoverPolicy | null): FailoverDraft {
	return {
		max_targets: f?.max_targets ?? 3,
		attempts_per_target: text(f?.attempts_per_target),
		per_target_timeout_ms: text(f?.per_target_timeout_ms),
		deadline_ms: text(f?.deadline_ms),
		rate_limited: f?.on?.rate_limited ?? true,
		server_error: f?.on?.server_error ?? true,
		connect_error: f?.on?.connect_error ?? true,
		limit_refused: f?.on?.limit_refused ?? true,
		timeout: f?.on?.timeout ?? false
	};
}

export function failoverOf(d: FailoverDraft): FailoverPolicy {
	return {
		max_targets: d.max_targets,
		attempts_per_target: numberOrNull(d.attempts_per_target),
		per_target_timeout_ms: numberOrNull(d.per_target_timeout_ms),
		deadline_ms: numberOrNull(d.deadline_ms),
		on: {
			rate_limited: d.rate_limited,
			server_error: d.server_error,
			connect_error: d.connect_error,
			limit_refused: d.limit_refused,
			timeout: d.timeout
		}
	};
}

export function actionDraft(a?: RouteAction | null): ActionDraft {
	return {
		tiers: (a?.tiers ?? []).join(', '),
		algorithm: a?.algorithm ?? 'priority',
		affinity: a?.affinity ?? 'none',
		overflow: a?.overflow ?? 'none',
		failover: a?.failover ? failoverDraft(a.failover) : null
	};
}

export function actionOf(d: ActionDraft): RouteAction {
	const out: RouteAction = { algorithm: d.algorithm, affinity: d.affinity, overflow: d.overflow };
	const tiers = split(d.tiers);
	if (tiers.length > 0) out.tiers = tiers;
	if (d.failover) out.failover = failoverOf(d.failover);
	return out;
}

export function emptyRule(n: number): RuleDraft {
	return {
		name: `rule-${n}`,
		complexity: [],
		agent_id: '',
		task: '',
		user_id: '',
		template: '',
		tags: '',
		at_least: '',
		below: '',
		cel: '',
		then: actionDraft()
	};
}

export function schemeDraft(s?: Partial<RoutingScheme> | null): SchemeDraft {
	return {
		name: s?.name ?? '',
		description: s?.description ?? '',
		status: s?.status ?? 'active',
		rules: (s?.rules ?? []).map((r) => ({
			name: r.name,
			complexity: r.when?.complexity ?? [],
			agent_id: formatMatch(r.when?.agent_id),
			task: formatMatch(r.when?.task),
			user_id: formatMatch(r.when?.user_id),
			template: formatMatch(r.when?.template),
			tags: formatTags(r.when?.tags),
			at_least: text(r.when?.input_tokens?.at_least),
			below: text(r.when?.input_tokens?.below),
			cel: r.cel ?? '',
			then: actionDraft(r.then)
		})),
		default: actionDraft(s?.default),
		failover: failoverDraft(s?.failover)
	};
}

/** The scheme the API takes, from the editor's draft. Conditions not given are left out. */
export function schemeOf(d: SchemeDraft): RoutingScheme {
	return {
		name: d.name.trim(),
		description: d.description,
		status: d.status,
		default: actionOf(d.default),
		failover: failoverOf(d.failover),
		rules: d.rules.map((r) => {
			const when: RouteRule['when'] = {};
			if (r.complexity.length > 0) when.complexity = r.complexity;
			for (const key of ['agent_id', 'task', 'user_id', 'template'] as const) {
				const m = parseMatch(r[key]);
				if (Object.keys(m).length > 0) when[key] = m;
			}
			const tags = parseTags(r.tags);
			if (Object.keys(tags).length > 0) when.tags = tags;
			const atLeast = numberOrNull(r.at_least);
			const below = numberOrNull(r.below);
			if (atLeast !== null || below !== null) {
				when.input_tokens = {
					...(atLeast !== null ? { at_least: atLeast } : {}),
					...(below !== null ? { below } : {})
				};
			}
			const rule: RouteRule = { name: r.name.trim(), when, then: actionOf(r.then) };
			if (r.cel.trim()) rule.cel = r.cel.trim();
			return rule;
		})
	};
}

/** A rule's condition in words, for lists. */
export function describeWhen(rule: RouteRule): string {
	const w = rule.when ?? {};
	const parts: string[] = [];
	if (w.complexity?.length) parts.push(`complexity ${w.complexity.join(' or ')}`);
	const named: [string, StrMatch | undefined][] = [
		['agent', w.agent_id],
		['task', w.task],
		['user', w.user_id],
		['template', w.template]
	];
	for (const [label, m] of named) {
		const line = formatMatch(m);
		if (line === '*') parts.push(`${label} given`);
		else if (line === '-') parts.push(`no ${label}`);
		else if (line) parts.push(`${label} ${line}`);
	}
	for (const [k, m] of Object.entries(w.tags ?? {}))
		parts.push(`tag ${k} ${formatMatch(m) || 'any'}`);
	if (w.input_tokens?.at_least !== undefined && w.input_tokens.at_least !== null)
		parts.push(`input ≥ ${w.input_tokens.at_least} tokens`);
	if (w.input_tokens?.below !== undefined && w.input_tokens.below !== null)
		parts.push(`input < ${w.input_tokens.below} tokens`);
	if (rule.cel) parts.push(`CEL: ${rule.cel}`);
	return parts.length > 0 ? parts.join('; ') : 'every call';
}

/** An action in words: "tiers small, large · weighted". */
export function describeAction(a: RouteAction | undefined): string {
	const tiers = a?.tiers?.length ? `tiers ${a.tiers.join(', ')}` : 'the whole pool';
	const algorithm = ALGORITHMS.find((x) => x.value === (a?.algorithm ?? 'priority'))?.label ?? '';
	const extra = [
		a?.affinity === 'session' ? 'sessions stay' : '',
		a?.overflow === 'rest_of_pool' ? 'then the rest of the pool' : ''
	].filter(Boolean);
	return [tiers, algorithm.toLowerCase(), ...extra].join(' · ');
}
