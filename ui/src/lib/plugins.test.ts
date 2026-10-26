import { describe, expect, it } from 'vitest';
import type { Schemas } from '#lib/api/types.ts';
import {
	bindingPrepare,
	cacheSettingsText,
	cacheSourceLabel,
	clearedMessage,
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
		expect(
			settingsSummary(
				binding('response_cache', { calls: 'all', scope: 'workspace', ttl_seconds: 60 })
			)
		).toBe('all calls, kept 60 s, shared by the workspace');
		expect(settingsSummary(binding('response_cache', { scope: 'end_user' }))).toBe(
			'deterministic calls, kept 3600 s, per end user'
		);
		expect(settingsSummary(binding('response_cache', {}))).toBe(
			'deterministic calls, kept 3600 s, per key'
		);
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

describe('the response cache of a scope', () => {
	it('says where its binding comes from and what it keeps', () => {
		expect(cacheSourceLabel('workspace')).toBe('The workspace’s own binding');
		expect(cacheSourceLabel('platform')).toMatch(/^The platform’s binding/);
		expect(cacheSourceLabel('other')).toBe('other');
		expect(cacheSettingsText({ plugin: 'response_cache', calls: 'all', ttl_seconds: 60 })).toBe(
			'all calls, kept 60 s, per key'
		);
		expect(cacheSettingsText(null)).toBe('deterministic calls, kept 3600 s, per key');
	});

	it('says what a clear did and how the space is freed', () => {
		const result = (scope: string, reclaim: string) =>
			({ scope, reclaim, cleared_at: '2026-10-25T10:00:00Z' }) as Schemas['ClearResult'];
		expect(clearedMessage(result('workspace', 'per_instance'))).toBe(
			'The workspace’s response cache was cleared. Answers kept before now are misses, and the next calls are answered by their providers. Each gateway frees their memory as it applies the clear.'
		);
		expect(clearedMessage(result('organisation', 'not_available'))).toMatch(
			/stay in Valkey until they expire/
		);
		expect(clearedMessage(result('platform', 'started'), ' Applied on 1 of 2.')).toMatch(
			/^The response cache of every workspace was cleared\..*removed from Valkey\. Applied on 1 of 2\.$/
		);
	});
});
