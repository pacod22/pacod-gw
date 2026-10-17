import { resource } from '#lib/server/resource.ts';

const catalogues = resource({
	path: '/orgs/{org}/workspaces/{ws}/intent-catalogues',
	noun: 'intent catalogue'
});

export const load = catalogues.load;
export const actions = catalogues.actions;
