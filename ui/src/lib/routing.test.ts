import { describe, expect, it } from 'vitest';
import {
	describeAction,
	describeWhen,
	emptyRule,
	formatMatch,
	loadedSchemesMessage,
	parseMatch,
	planSchemes,
	schemeDraft,
	schemeOf,
	type RoutingScheme
} from './routing';

describe('a condition on one value', () => {
	it('reads and writes its one-line form', () => {
		expect(parseMatch('')).toEqual({});
		expect(parseMatch(' * ')).toEqual({ present: true });
		expect(parseMatch('-')).toEqual({ present: false });
		expect(parseMatch('review, ci-*, ,triage')).toEqual({
			in: ['review', 'triage'],
			prefix: ['ci-']
		});
		for (const line of ['', '*', '-', 'review, triage, ci-*']) {
			expect(formatMatch(parseMatch(line))).toBe(line);
		}
	});
});

describe('a scheme in the editor', () => {
	const scheme: RoutingScheme = {
		name: 'tiers',
		description: 'By complexity',
		status: 'active',
		rules: [
			{
				name: 'cheap',
				when: {
					complexity: ['low', 'unset'],
					agent_id: { prefix: ['triage-'] },
					tags: { team: { in: ['search', 'ads'] }, site: { present: true } },
					input_tokens: { below: 8000 }
				},
				then: { tiers: ['small'], algorithm: 'weighted', affinity: 'session', overflow: 'none' }
			},
			{
				name: 'long',
				when: {},
				cel: 'input_tokens > 32000',
				then: {
					tiers: ['large', 'small'],
					algorithm: 'priority',
					affinity: 'none',
					overflow: 'rest_of_pool',
					failover: {
						max_targets: 2,
						attempts_per_target: 1,
						per_target_timeout_ms: null,
						deadline_ms: 30000,
						on: {
							rate_limited: true,
							server_error: true,
							connect_error: true,
							limit_refused: false,
							timeout: true
						}
					}
				}
			}
		],
		default: { tiers: ['large'], algorithm: 'priority', affinity: 'none', overflow: 'none' },
		failover: {
			max_targets: 3,
			attempts_per_target: null,
			per_target_timeout_ms: null,
			deadline_ms: null,
			on: {
				rate_limited: true,
				server_error: true,
				connect_error: true,
				limit_refused: true,
				timeout: false
			}
		}
	};

	it('comes back as it went in', () => {
		expect(schemeOf(schemeDraft(scheme))).toEqual(scheme);
	});

	it('leaves out what a new rule does not say', () => {
		const draft = schemeDraft({ name: 'x' });
		draft.rules.push(emptyRule(1));
		const out = schemeOf(draft);
		expect(out.rules[0]).toEqual({
			name: 'rule-1',
			when: {},
			then: { algorithm: 'priority', affinity: 'none', overflow: 'none' }
		});
	});

	it('describes rules in words', () => {
		expect(describeWhen(scheme.rules[0])).toBe(
			'complexity low or unset; agent triage-*; tag team search, ads; tag site *; input < 8000 tokens'
		);
		expect(describeWhen({ name: 'any', when: {}, then: scheme.default })).toBe('every call');
		expect(describeAction(scheme.rules[1].then)).toBe(
			'tiers large, small · main and backups · then the rest of the pool'
		);
	});
});

describe('loading the default schemes', () => {
	it('creates only the schemes whose name is free', () => {
		const defaults = [
			{ name: 'standard', description: 'Main and backups' },
			{ name: 'fastest', description: 'Lowest latency' }
		];
		expect(planSchemes(defaults, ['standard', 'mine'])).toEqual([
			{ name: 'standard', description: 'Main and backups', create: false },
			{ name: 'fastest', description: 'Lowest latency', create: true }
		]);
	});

	it('says what it created', () => {
		expect(loadedSchemesMessage(['fastest'])).toBe('Created the routing scheme “fastest”.');
		expect(loadedSchemesMessage(['a', 'b', 'c'])).toBe(
			'Created the routing schemes “a”, “b” and “c”.'
		);
		expect(loadedSchemesMessage([])).toMatch(/^Nothing to load/);
	});
});
