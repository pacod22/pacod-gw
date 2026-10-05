import { resource } from '#lib/server/resource.ts';

const organisations = resource({ path: '/orgs', noun: 'organisation' });

export const load = organisations.load;
export const actions = organisations.actions;
