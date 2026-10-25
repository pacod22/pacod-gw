import { installPrepare } from '#lib/plugins.ts';
import { compiledPlugins, installFields } from '#lib/server/plugins.ts';
import { resource } from '#lib/server/resource.ts';

const installs = resource({
	path: '/plugin-installs',
	noun: 'plugin install',
	fields: installFields,
	prepare: installPrepare,
	extra: compiledPlugins
});

export const load = installs.load;
export const actions = installs.actions;
