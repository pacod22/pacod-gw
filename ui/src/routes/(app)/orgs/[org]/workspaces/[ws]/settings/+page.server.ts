import { objectResource } from '#lib/server/resource.ts';

// The settings of a workspace are part of the workspace object.
const workspace = objectResource({
	path: '/orgs/{org}/workspaces/{ws}',
	noun: 'workspace',
	fields: {
		// Locking settings is for organisations; the workspace schema shares the type.
		'settings.locked': { hidden: true },
		// So is ruling audit mode out or making it mandatory.
		'settings.audit.rule': { hidden: true }
	}
});

export const load = workspace.load;
export const actions = workspace.actions;
