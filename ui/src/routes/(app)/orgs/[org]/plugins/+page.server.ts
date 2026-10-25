import { bindingPrepare } from '#lib/plugins.ts';
import { bindingFields, bindingTemplate, compiledPlugins } from '#lib/server/plugins.ts';
import { resource } from '#lib/server/resource.ts';

const bindings = resource({
	path: '/orgs/{org}/plugin-bindings',
	noun: 'plugin binding',
	fields: (event) => bindingFields(event, event.params.org ?? '', null),
	prepare: bindingPrepare,
	extra: async (event) => {
		const [template, plugins] = await Promise.all([
			bindingTemplate(event, 'organisation'),
			compiledPlugins(event)
		]);
		return { ...template, plugins: plugins.plugins };
	}
});

export const load = bindings.load;
export const actions = bindings.actions;
