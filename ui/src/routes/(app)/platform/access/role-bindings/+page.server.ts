import { organisationOptions, principalOptions, workspaceOptions } from '#lib/server/options.ts';
import { resource } from '#lib/server/resource.ts';

const bindings = resource({
	path: '/role-bindings',
	noun: 'role binding',
	nameOf: (binding) => `${binding.role} for ${binding.principal}`,
	// A binding refers to a principal and to a scope by id: offer the objects that exist.
	fields: async (event) => {
		const [principals, orgs, workspaces] = await Promise.all([
			principalOptions(event),
			organisationOptions(event),
			workspaceOptions(event)
		]);
		return {
			principal: { label: 'User or admin token', options: principals },
			'scope.id@organisation': { label: 'Organisation', options: orgs },
			'scope.id@workspace': {
				label: 'Workspace',
				...(workspaces.length > 0 ? { options: workspaces } : {})
			}
		};
	},
	extra: async (event) => {
		const [principals, orgs, workspaces] = await Promise.all([
			principalOptions(event),
			organisationOptions(event),
			workspaceOptions(event)
		]);
		const names = (options: { value: string; label: string }[]) =>
			Object.fromEntries(options.map((o) => [o.value, o.label]));
		return { principals: names(principals), scopes: { ...names(orgs), ...names(workspaces) } };
	}
});

export const load = bindings.load;
export const actions = bindings.actions;
