import { resource } from '#lib/server/resource.ts';
import { importTemplate } from '#lib/server/templates.ts';

// The list and the deletion come from the kit; a template is created and edited on pages
// of its own, because its form is not the generic one. There is no "last used" column:
// calls are counted per workspace by the template's name, which a workspace's own template
// may share.
const templates = resource({ path: '/orgs/{org}/templates', noun: 'template' });

export const load = templates.load;
export const actions = {
	delete: templates.actions.delete,
	import: importTemplate
};
