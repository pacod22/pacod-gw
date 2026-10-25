/**
 * What the console shows of plugin bindings (docs/plugin-concept.md): a binding's settings in
 * a few words, and the platform's bindings that apply where nothing else does.
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
		return `${setting(b, 'calls') || 'deterministic'} calls, kept ${setting(b, 'ttl_seconds') || '3600'} s`;
	}
	return '';
}

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
