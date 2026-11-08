import { resource } from '#lib/server/resource.ts';

const servers = resource({ path: '/orgs/{org}/tool-servers', noun: 'tool server' });

export const load = servers.load;
export const actions = servers.actions;
