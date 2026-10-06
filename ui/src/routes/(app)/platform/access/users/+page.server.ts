import { resource } from '#lib/server/resource.ts';
import { loadRoles } from '#lib/server/roles.ts';

// Creating, changing and deleting a user happen on the user's own page.
const users = resource({ path: '/users', noun: 'user', extra: loadRoles });

export const load = users.load;
