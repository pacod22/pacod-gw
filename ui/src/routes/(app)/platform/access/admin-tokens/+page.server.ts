import { resource } from '#lib/server/resource.ts';

const tokens = resource({ path: '/admin-tokens', noun: 'admin token' });

export const load = tokens.load;
export const actions = tokens.actions;
