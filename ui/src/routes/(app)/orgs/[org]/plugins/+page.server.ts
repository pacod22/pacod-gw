import { bindingFields, bindingTemplate } from '#lib/server/plugins.ts';
import { resource } from '#lib/server/resource.ts';

const bindings = resource({
	path: '/orgs/{org}/plugin-bindings',
	noun: 'plugin binding',
	fields: (event) => bindingFields(event, event.params.org ?? '', null),
	extra: (event) => bindingTemplate(event, 'organisation')
});

export const load = bindings.load;
export const actions = bindings.actions;
