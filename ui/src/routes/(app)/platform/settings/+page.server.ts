import { objectResource } from '#lib/server/resource.ts';

const settings = objectResource({ path: '/settings', noun: 'settings' });

export const load = settings.load;
export const actions = settings.actions;
