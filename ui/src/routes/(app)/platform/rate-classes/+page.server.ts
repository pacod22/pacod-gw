import { resource } from '#lib/server/resource.ts';

const classes = resource({ path: '/rate-classes', noun: 'rate class' });

export const load = classes.load;
export const actions = classes.actions;
