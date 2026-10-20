/**
 * What the plugin pages share (docs/plugin-concept.md, section 4): the compiled plugins, the
 * catalogues a binding can name, and the form of a binding.
 */
import { canWrite } from '#lib/access.ts';
import type { Schemas } from '#lib/api/types.ts';
import type { FieldOverrides, Option } from '#lib/schema/form.ts';
import { problemOf, type KitEvent } from './resource.ts';

const LIMIT = 500;

/**
 * The built-in catalogue the form to add one starts from, for those who may add one here;
 * `null` for the others, or when it cannot be read: the form then starts empty.
 */
export async function catalogueTemplate(
	event: KitEvent,
	scope: 'organisation' | 'workspace'
): Promise<{ template: Schemas['Catalogue'] | null }> {
	const principal = event.locals.principal;
	if (principal === null || !canWrite(principal, scope, event.params)) return { template: null };
	const result = await event.locals.api.GET('/intent-catalogue-template');
	return { template: result.response.ok ? (result.data ?? null) : null };
}

/**
 * The intent binding the form to add one starts from, with what keeps it from being created
 * as it is; `null` for those who may not add one, or when it cannot be read.
 */
export async function bindingTemplate(
	event: KitEvent,
	scope: 'organisation' | 'workspace'
): Promise<{ bindingTemplate: Schemas['BindingTemplate'] | null }> {
	const principal = event.locals.principal;
	if (principal === null || !canWrite(principal, scope, event.params)) {
		return { bindingTemplate: null };
	}
	const org = event.params.org ?? '';
	const result =
		scope === 'workspace'
			? await event.locals.api.GET('/orgs/{org}/workspaces/{ws}/plugin-binding-template', {
					params: { path: { org, ws: event.params.ws ?? '' } }
				})
			: await event.locals.api.GET('/orgs/{org}/plugin-binding-template', {
					params: { path: { org } }
				});
	return { bindingTemplate: result.response.ok ? (result.data ?? null) : null };
}

/**
 * A text and a labelled set to try the workspace's intent binding with, made for its
 * catalogue; `null` without a binding, or for those who may not try it.
 */
export async function intentDemo(event: KitEvent): Promise<{ demo: Schemas['Demo'] | null }> {
	const principal = event.locals.principal;
	if (principal === null || !canWrite(principal, 'workspace', event.params)) return { demo: null };
	const result = await event.locals.api.GET('/orgs/{org}/workspaces/{ws}/plugins/intent/demo', {
		params: { path: { org: event.params.org ?? '', ws: event.params.ws ?? '' } }
	});
	return { demo: result.response.ok ? (result.data ?? null) : null };
}

/** The plugins compiled into the gateway, and whether the platform installed them. */
export async function compiledPlugins(event: KitEvent) {
	const result = await event.locals.api.GET('/plugins');
	return {
		plugins: (result.data ?? []) as Schemas['PluginInfo'][],
		pluginsProblem: result.response.ok ? null : problemOf(event, result)
	};
}

function pluginOptions(plugins: Schemas['PluginInfo'][]): Option[] {
	return plugins.map((p) => ({
		value: p.id,
		label: `${p.title} (${p.id})`,
		help: p.installed ? (p.enabled ? '' : 'Installed, but switched off.') : 'Not installed yet.'
	}));
}

/** The catalogues a binding of this scope can name: the workspace's own, then its organisation's. */
async function catalogueOptions(
	event: KitEvent,
	org: string,
	ws: string | null
): Promise<Option[]> {
	const [own, inherited] = await Promise.all([
		ws
			? event.locals.api.GET('/orgs/{org}/workspaces/{ws}/intent-catalogues', {
					params: { path: { org, ws }, query: { limit: LIMIT } }
				})
			: Promise.resolve(null),
		event.locals.api.GET('/orgs/{org}/intent-catalogues', {
			params: { path: { org }, query: { limit: LIMIT } }
		})
	]);
	const seen = new Set<string>();
	const options: Option[] = [];
	for (const [list, from] of [
		[own?.data?.items ?? [], 'this workspace'],
		[inherited.data?.items ?? [], 'the organisation']
	] as const) {
		for (const c of list) {
			if (seen.has(c.name)) continue;
			seen.add(c.name);
			options.push({
				value: c.name,
				label: c.name,
				help: `Of ${from}. ${c.description ?? ''}`.trim()
			});
		}
	}
	return options;
}

/** The form of a binding: which plugin, and the names a binding refers to. */
export async function bindingFields(
	event: KitEvent,
	org: string,
	ws: string | null
): Promise<FieldOverrides> {
	const [{ plugins }, catalogues] = await Promise.all([
		compiledPlugins(event),
		catalogueOptions(event, org, ws)
	]);
	return {
		plugin: { label: 'Plugin', options: pluginOptions(plugins), order: -10 },
		mode: {
			help: 'Shadow looks at calls and records what it would do, without changing any; enforce acts on its verdict.',
			order: -9
		},
		'settings.catalogue': {
			label: 'Intent catalogue',
			options: catalogues,
			help: 'The workspace’s own catalogue of that name, else its organisation’s.'
		},
		'settings.llm.model': {
			label: 'Classifying model',
			help: 'A model id or a @template the scope may use; the model must offer JSON-schema output.'
		},
		'settings.jev.model': {
			label: 'Jev model',
			help: 'A decision model of the repository, for example typesafe/jev-1.13.0.'
		},
		...(ws ? { locked: { hidden: true } } : {})
	};
}
