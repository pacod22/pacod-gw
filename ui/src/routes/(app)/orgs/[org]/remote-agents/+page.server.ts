import { resource } from '#lib/server/resource.ts';

const agents = resource({ path: '/orgs/{org}/remote-agents', noun: 'remote agent' });

export const load = agents.load;
export const actions = agents.actions;
