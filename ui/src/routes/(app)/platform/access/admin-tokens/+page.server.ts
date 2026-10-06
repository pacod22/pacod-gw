import { resource } from '#lib/server/resource.ts';
import { loadRoles, roleActions } from '#lib/server/roles.ts';

const tokens = resource({ path: '/admin-tokens', noun: 'admin token', extra: loadRoles });

export const load = tokens.load;
export const actions = { ...tokens.actions, ...roleActions };
