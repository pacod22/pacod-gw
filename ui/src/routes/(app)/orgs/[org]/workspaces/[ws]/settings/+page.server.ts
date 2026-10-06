import { objectResource } from '#lib/server/resource.ts';
import { schemeOptions } from '#lib/server/routing.ts';

// The settings of a workspace are part of the workspace object.
const workspace = objectResource({
	path: '/orgs/{org}/workspaces/{ws}',
	noun: 'workspace',
	fields: async (event) => ({
		// Locking settings is for organisations; the workspace schema shares the type.
		'settings.locked': { hidden: true },
		// So is ruling audit mode out or making it mandatory.
		'settings.audit.rule': { hidden: true },
		'settings.routing_scheme': {
			label: 'Routing scheme',
			help: 'For templates with several models that name no scheme of their own. Unset: the organisation’s.',
			options: await schemeOptions(event, event.params.org ?? '')
		}
	})
});

export const load = workspace.load;
export const actions = workspace.actions;
