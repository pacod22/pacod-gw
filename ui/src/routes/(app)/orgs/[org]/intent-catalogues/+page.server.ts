import { resource } from '#lib/server/resource.ts';

const catalogues = resource({
	path: '/orgs/{org}/intent-catalogues',
	noun: 'intent catalogue'
});

export const load = catalogues.load;
export const actions = catalogues.actions;
