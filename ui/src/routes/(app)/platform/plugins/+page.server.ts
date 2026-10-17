import { compiledPlugins } from '#lib/server/plugins.ts';
import { resource } from '#lib/server/resource.ts';

const installs = resource({
	path: '/plugin-installs',
	noun: 'plugin install',
	fields: async (event) => {
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
				options: [...new Set(engines.flatMap(({ engine }) => engine.third_party ?? []))].map(
					(t) => ({
						value: t,
						label: t,
						help: ''
					})
				)
			}
		};
	},
	extra: compiledPlugins
});

export const load = installs.load;
export const actions = installs.actions;
