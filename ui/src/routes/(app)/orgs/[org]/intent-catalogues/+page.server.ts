import { catalogueTemplate } from '#lib/server/plugins.ts';
import { resource } from '#lib/server/resource.ts';

const catalogues = resource({
	path: '/orgs/{org}/intent-catalogues',
	noun: 'intent catalogue',
	extra: (event) => catalogueTemplate(event, 'organisation')
});

export const load = catalogues.load;
export const actions = catalogues.actions;
