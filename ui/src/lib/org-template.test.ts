import { describe, expect, it } from 'vitest';
import {
	describeSetting,
	fillSettings,
	loadedMessage,
	planTemplate,
	type OrgConfigTemplate
} from './org-template';

type Scheme = OrgConfigTemplate['routing_schemes'][number];

const template: OrgConfigTemplate = {
	settings: {
		unsupported_params: 'drop',
		audit: {
			mode: 'on_request',
			retention_days: 14,
			attachments: 'reference',
			guarantee: 'best_effort'
		},
		routing_scheme: 'standard'
	},
	routing_schemes: [
		// Only the name and the description matter here.
		{ name: 'standard', description: 'Main and backups' } as Scheme,
		{ name: 'spread-load', description: 'By weight' } as Scheme
	],
	reasons: {
		unsupported_params: 'keeps calls working',
		audit: 'for debugging',
		routing_scheme: 'one place'
	}
};

describe('loading the organisation template', () => {
	it('fills an empty organisation completely', () => {
		const plan = planTemplate(template, {}, []);
		expect(plan.changes).toBe(5);
		expect(plan.settings.map((s) => [s.label, s.now, s.value, s.apply])).toEqual([
			['Unsupported parameters', null, 'Drop and report', true],
			['Audit mode', null, 'On request, attachments as size and hash, kept 14 days', true],
			['Routing scheme', null, 'standard', true]
		]);
		expect(plan.settings[1].reason).toBe('for debugging');
		expect(plan.schemes.every((s) => s.create)).toBe(true);
	});

	it('keeps what the organisation already has', () => {
		const current = {
			unsupported_params: 'reject' as const,
			audit: {
				mode: 'on' as const,
				rule: 'force' as const,
				attachments: 'full' as const,
				guarantee: 'best_effort' as const
			},
			locked: ['audit']
		};
		const plan = planTemplate(template, current, ['standard']);
		expect(plan.changes).toBe(2);
		expect(plan.settings.find((s) => s.key === 'audit')).toMatchObject({
			now: 'Every call, mandatory for workspaces',
			apply: false
		});
		expect(plan.schemes.map((s) => [s.name, s.create])).toEqual([
			['standard', false],
			['spread-load', true]
		]);

		const filled = fillSettings(template, current);
		expect(filled).toEqual({ ...current, routing_scheme: 'standard' });
		// Loading it again changes nothing.
		expect(planTemplate(template, filled, ['standard', 'spread-load']).changes).toBe(0);
	});

	it('says what it did', () => {
		expect(loadedMessage(['standard', 'spread-load'], ['Audit mode'])).toBe(
			'Template loaded: created the routing schemes “standard” and “spread-load”; set audit mode.'
		);
		expect(loadedMessage([], [])).toMatch(/^Nothing to load/);
		expect(describeSetting('provider_tools', [])).toBe('None');
		expect(describeSetting('locked', ['routing_scheme'])).toBe('Routing scheme');
	});
});
