/**
 * What the plugin pages share (docs/plugin-concept.md, section 4): the compiled plugins, the
 * catalogues a binding can name, the form of a binding, and the response cache of a scope.
 */
import { canWrite } from '#lib/access.ts';
import type { Schemas } from '#lib/api/types.ts';
import { clearedMessage } from '#lib/plugins.ts';
import type { FieldOverrides, Option } from '#lib/schema/form.ts';
import { appliedNote, done, failed, problemOf, type KitEvent } from './resource.ts';

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

/** The settings fields of the two plugins, wherever a binding sits in a form. */
function settingsFields(prefix: string, catalogues: Option[]): FieldOverrides {
	return {
		[`${prefix}settings`]: {
			label: 'Plugin',
			help: 'The plugin, and its settings.',
			required: true,
			order: -10
		},
		[`${prefix}settings.catalogue`]: {
			label: 'Intent catalogue',
			options: catalogues,
			help: 'The workspace’s own catalogue of that name, else its organisation’s.'
		},
		[`${prefix}settings.llm.model`]: {
			label: 'Classifying model',
			help: 'A model id or a @template the scope may use; the model must offer JSON-schema output.'
		},
		[`${prefix}settings.jev.model`]: {
			label: 'Jev model',
			help: 'A decision model of the repository, for example typesafe/jev-1.13.0.'
		},
		[`${prefix}settings.calls`]: {
			label: 'Calls answered from the cache',
			help: 'deterministic: calls with temperature 0 (the one the provider is sent, or the one the caller asked for where the model takes none). all: every call.'
		},
		[`${prefix}settings.ttl_seconds`]: {
			label: 'Lifetime of an answer (seconds)'
		},
		[`${prefix}settings.max_answer_bytes`]: {
			label: 'Largest answer kept (bytes)'
		}
	};
}

/** The form of a binding: which plugin, and the names a binding refers to. */
export async function bindingFields(
	event: KitEvent,
	org: string,
	ws: string | null
): Promise<FieldOverrides> {
	const catalogues = await catalogueOptions(event, org, ws);
	return {
		// Chosen with the settings (`bindingPrepare`).
		plugin: { hidden: true },
		mode: {
			help: 'Shadow looks at calls and records what it would do, without changing any; enforce acts on its verdict. For the response cache: enforce answers from the cache, shadow only records what it would have answered.',
			order: -9
		},
		...settingsFields('', catalogues),
		...(ws ? { locked: { hidden: true } } : {})
	};
}

/**
 * The form of a platform install. Its default binding applies to every workspace whose
 * organisation and own have none; it names the install's plugin.
 */
export async function installFields(event: KitEvent): Promise<FieldOverrides> {
	const { plugins } = await compiledPlugins(event);
	const engines = plugins.flatMap((p) => p.engines.map((e) => ({ plugin: p, engine: e })));
	return {
		plugin: {
			label: 'Plugin',
			options: plugins.map((p) => ({
				value: p.id,
				label: `${p.title} (${p.id})`,
				help: p.description
			}))
		},
		engines: {
			label: 'Engines allowed',
			help: 'None chosen: every engine of the plugin.',
			options: engines.map(({ plugin, engine }) => ({
				value: engine.id,
				label: `${engine.id} (${plugin.id})`,
				help: ''
			}))
		},
		third_parties: {
			label: 'Approved third parties',
			help: 'Engines that send text to a third party can be bound only once it is approved here.',
			options: [...new Set(engines.flatMap(({ engine }) => engine.third_party ?? []))].map((t) => ({
				value: t,
				label: t,
				help: ''
			}))
		},
		default_binding: {
			label: 'The platform’s binding',
			help: 'Applies to every workspace whose organisation and own bind the plugin otherwise not. Only for plugins whose settings name nothing of an organisation, such as the response cache.'
		},
		'default_binding.plugin': { hidden: true },
		'default_binding.locked': { hidden: true },
		...settingsFields('default_binding.', [])
	};
}

type CacheScope = 'platform' | 'organisation' | 'workspace';

/**
 * The response cache of an organisation or a workspace, for the card on its Plugins page, and
 * whether the principal may clear it. `null` when it cannot be read: the card is then left out.
 */
export async function scopeCache(
	event: KitEvent,
	scope: 'organisation' | 'workspace'
): Promise<{ cache: Schemas['ScopeCacheView'] | null; canClearCache: boolean }> {
	const org = event.params.org ?? '';
	const result =
		scope === 'workspace'
			? await event.locals.api.GET('/orgs/{org}/workspaces/{ws}/response-cache', {
					params: { path: { org, ws: event.params.ws ?? '' } }
				})
			: await event.locals.api.GET('/orgs/{org}/response-cache', { params: { path: { org } } });
	const principal = event.locals.principal;
	return {
		cache: result.response.ok ? (result.data ?? null) : null,
		canClearCache: principal !== null && canWrite(principal, scope, event.params)
	};
}

/** The form action that clears the response cache of the page's scope. */
export async function clearCache(event: KitEvent, scope: CacheScope) {
	const org = event.params.org ?? '';
	const result =
		scope === 'workspace'
			? await event.locals.api.POST('/orgs/{org}/workspaces/{ws}/response-cache/clear', {
					params: { path: { org, ws: event.params.ws ?? '' } }
				})
			: scope === 'organisation'
				? await event.locals.api.POST('/orgs/{org}/response-cache/clear', {
						params: { path: { org } }
					})
				: await event.locals.api.POST('/response-cache/clear');
	if (!result.response.ok || !result.data) return failed(event, 'clearCache', result);
	return done('clearCache', clearedMessage(result.data, appliedNote(result.response)));
}
