import { bindingPrepare } from '#lib/plugins.ts';
import {
	bindingFields,
	bindingTemplate,
	clearCache,
	compiledPlugins,
	scopeCache
} from '#lib/server/plugins.ts';
import { resource, type KitEvent } from '#lib/server/resource.ts';

const bindings = resource({
	path: '/orgs/{org}/plugin-bindings',
	noun: 'plugin binding',
	fields: (event) => bindingFields(event, event.params.org ?? '', null),
	prepare: bindingPrepare,
	extra: async (event) => {
		const [template, plugins, cache] = await Promise.all([
			bindingTemplate(event, 'organisation'),
			compiledPlugins(event),
			scopeCache(event, 'organisation')
		]);
		return { ...template, plugins: plugins.plugins, ...cache };
	}
});

export const load = bindings.load;
export const actions = {
	...bindings.actions,
	clearCache: (event: KitEvent) => clearCache(event, 'organisation')
};
