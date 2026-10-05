import { resource } from '#lib/server/resource.ts';

const workspaces = resource({
	path: '/orgs/{org}/workspaces',
	noun: 'workspace',
	fields: {
		// Locking settings is for organisations; the workspace schema shares the type.
		'settings.locked': { hidden: true },
		// So is ruling audit mode out or making it mandatory.
		'settings.audit.rule': { hidden: true }
	}
});

export const load = workspaces.load;
export const actions = workspaces.actions;
