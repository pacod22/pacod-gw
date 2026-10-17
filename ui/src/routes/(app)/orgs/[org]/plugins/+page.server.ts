import { bindingFields } from '#lib/server/plugins.ts';
import { resource } from '#lib/server/resource.ts';

const bindings = resource({
	path: '/orgs/{org}/plugin-bindings',
	noun: 'plugin binding',
	fields: (event) => bindingFields(event, event.params.org ?? '', null)
});

export const load = bindings.load;
export const actions = bindings.actions;
