import { resource, type KitEvent } from '#lib/server/resource.ts';

// Creating asks only for the names; everything else is set on the workspace's Settings page,
// which is also where it is changed and deleted.
const workspaces = resource({
	path: '/orgs/{org}/workspaces',
	noun: 'workspace',
	fields: {
		display_name: {
			label: 'Display name',
			help: 'How the console shows the workspace, for example “Billing service”.',
			required: true,
			order: -1
		},
		name: {
			label: 'Name',
			help: 'Lower-case letters, digits and hyphens; unique in the organisation. Filled in from the display name.',
			slugFrom: 'display_name'
		},
		status: { hidden: true },
		settings: { hidden: true },
		tag_keys: { hidden: true },
		time_zone: { hidden: true }
	}
});

export const load = async (event: KitEvent) => {
	const data = await workspaces.load(event);
	return { ...data, replaceFields: null, canDelete: false };
};

export const actions = { create: workspaces.actions.create };
