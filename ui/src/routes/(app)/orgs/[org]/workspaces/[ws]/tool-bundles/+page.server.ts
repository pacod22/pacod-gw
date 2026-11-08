import { resource } from '#lib/server/resource.ts';

const bundles = resource({
	path: '/orgs/{org}/workspaces/{ws}/tool-bundles',
	noun: 'tool bundle',
	fields: async () => ({
		allow: {
			help: 'mcp:<server>/* grants every approved tool of a tool server; mcp:<server>/<tool> one tool, by its own name on the server; a2a:<agent> a remote agent.'
		},
		deny: { help: 'Entries in the same form that the bundle never grants. A deny entry wins.' }
	})
});

export const load = bundles.load;
export const actions = bundles.actions;
