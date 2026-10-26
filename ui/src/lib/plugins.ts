/**
 * What the console shows of plugin bindings (docs/plugin-concept.md): a binding's settings in
 * a few words, the platform's bindings that apply where nothing else does, and the response
 * cache of a scope.
 */
import type { Schemas } from '#lib/api/types.ts';

type Binding = Schemas['PluginBinding'];

function setting(b: Binding, name: string): string {
	const value = (b.settings as Record<string, unknown> | null)?.[name];
	return value === undefined || value === null ? '' : String(value);
}

/** A binding's settings in a few words, for a list. */
export function settingsSummary(b: Binding): string {
	if (b.plugin === 'intent') {
		const catalogue = setting(b, 'catalogue');
		return `${setting(b, 'engine') || 'llm'}${catalogue ? `, catalogue ${catalogue}` : ''}`;
	}
	if (b.plugin === 'response_cache') {
		const scope = CACHE_SCOPES[setting(b, 'scope') || 'key'] ?? setting(b, 'scope');
		return `${setting(b, 'calls') || 'deterministic'} calls, kept ${setting(b, 'ttl_seconds') || '3600'} s, ${scope}`;
	}
	return '';
}

/** Whose calls share the response cache's answers, as a binding's scope says. */
const CACHE_SCOPES: Record<string, string> = {
	workspace: 'shared by the workspace',
	key: 'per key',
	end_user: 'per end user'
};

/** What the intent plugin's `on_error` and `expose` mean; other plugins have neither. */
export function intentOnly(b: Binding, value: string): string {
	return b.plugin === 'intent' ? value : '–';
}

/** The plugins the platform binds for every workspace that binds them otherwise not. */
export function platformBound(plugins: Schemas['PluginInfo'][]): string[] {
	return plugins
		.filter((p) => p.platform_mode && p.platform_mode !== 'off')
		.map((p) => `${p.title} (${p.platform_mode})`);
}

/**
 * A binding's plugin is chosen once, with its settings (`settings.plugin`): the form asks for
 * it there, and the binding names it again.
 */
export function bindingPrepare(body: Record<string, unknown>): Record<string, unknown> {
	const settings = body.settings;
	if (typeof settings === 'object' && settings !== null && 'plugin' in settings) {
		return { ...body, plugin: (settings as { plugin: unknown }).plugin };
	}
	return body;
}

/** An install's default binding names the install's plugin. */
export function installPrepare(body: Record<string, unknown>): Record<string, unknown> {
	const binding = body.default_binding;
	if (typeof binding === 'object' && binding !== null) {
		return { ...body, default_binding: { ...binding, plugin: body.plugin } };
	}
	return body;
}

/** Where the response cache's binding for a scope comes from, in words. */
export function cacheSourceLabel(source: string): string {
	switch (source) {
		case 'workspace':
			return 'The workspace’s own binding';
		case 'organisation':
			return 'The organisation’s binding';
		case 'platform':
			return 'The platform’s binding, which every workspace gets unless it has its own';
		default:
			return source;
	}
}

/** The response cache's settings in a few words. */
export function cacheSettingsText(settings: unknown): string {
	return settingsSummary({ plugin: 'response_cache', settings } as unknown as Binding);
}

/** What a clear did, for the notice after it. `applied` is the note on the other instances. */
export function clearedMessage(result: Schemas['ClearResult'], applied = ''): string {
	const what =
		result.scope === 'platform'
			? 'The response cache of every workspace was cleared.'
			: result.scope === 'organisation'
				? 'The response cache of every workspace of the organisation was cleared.'
				: 'The workspace’s response cache was cleared.';
	const space =
		result.reclaim === 'started'
			? ' Their keys are being removed from Valkey.'
			: result.reclaim === 'not_available'
				? ' Their keys stay in Valkey until they expire: this instance does not reach it.'
				: ' Each gateway frees their memory as it applies the clear.';
	return `${what} Answers kept before now are misses, and the next calls are answered by their providers.${space}${applied}`;
}
