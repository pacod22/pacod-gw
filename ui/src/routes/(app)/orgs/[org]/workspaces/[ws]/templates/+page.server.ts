import { resource } from '#lib/server/resource.ts';

// The list and the deletion come from the kit; a template is created and edited on pages
// of its own, because its form is not the generic one.
const templates = resource({
	path: '/orgs/{org}/workspaces/{ws}/templates',
	noun: 'template'
});

export const load = templates.load;
export const actions = { delete: templates.actions.delete };
