import { describe, expect, it } from 'vitest';
import type { Schemas } from '#lib/api/types.ts';
import {
	bindingPrepare,
	installPrepare,
	intentOnly,
	platformBound,
	settingsSummary
} from './plugins.ts';

const binding = (plugin: string, settings: Record<string, unknown>) =>
	({ plugin, mode: 'enforce', settings }) as unknown as Schemas['PluginBinding'];

describe('plugin bindings in a list', () => {
	it('sums up each plugin’s settings', () => {
		expect(settingsSummary(binding('intent', { engine: 'jev', catalogue: 'support' }))).toBe(
			'jev, catalogue support'
		);
		expect(settingsSummary(binding('intent', {}))).toBe('llm');
		expect(settingsSummary(binding('response_cache', { calls: 'all', ttl_seconds: 60 }))).toBe(
			'all calls, kept 60 s'
		);
		expect(settingsSummary(binding('response_cache', {}))).toBe('deterministic calls, kept 3600 s');
	});

	it('shows the intent plugin’s own settings for it alone', () => {
		expect(intentOnly(binding('intent', {}), 'closed')).toBe('closed');
		expect(intentOnly(binding('response_cache', {}), 'closed')).toBe('–');
	});

	it('names the plugins the platform binds everywhere', () => {
		const info = (title: string, platform_mode?: string) =>
			({ title, platform_mode }) as unknown as Schemas['PluginInfo'];
		expect(
			platformBound([info('Intent'), info('Response cache', 'enforce'), info('X', 'off')])
		).toEqual(['Response cache (enforce)']);
	});
});

describe('what a binding form sends', () => {
	it('names the plugin chosen with the settings on the binding as well', () => {
		expect(
			bindingPrepare({ mode: 'enforce', settings: { plugin: 'response_cache', calls: 'all' } })
		).toEqual({
			mode: 'enforce',
			plugin: 'response_cache',
			settings: { plugin: 'response_cache', calls: 'all' }
		});
		expect(bindingPrepare({ mode: 'off' })).toEqual({ mode: 'off' });
	});

	it('names the install’s plugin on its default binding', () => {
		expect(
			installPrepare({
				plugin: 'response_cache',
				default_binding: { mode: 'shadow', settings: {} }
			})
		).toEqual({
			plugin: 'response_cache',
			default_binding: { plugin: 'response_cache', mode: 'shadow', settings: {} }
		});
		expect(installPrepare({ plugin: 'intent', default_binding: null })).toEqual({
			plugin: 'intent',
			default_binding: null
		});
	});
});
