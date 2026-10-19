import { catalogueTemplate } from '#lib/server/plugins.ts';
import { resource } from '#lib/server/resource.ts';

const catalogues = resource({
	path: '/orgs/{org}/workspaces/{ws}/intent-catalogues',
	noun: 'intent catalogue',
	extra: (event) => catalogueTemplate(event, 'workspace')
});

export const load = catalogues.load;
export const actions = catalogues.actions;
