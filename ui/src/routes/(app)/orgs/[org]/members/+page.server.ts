import { loadMembers, memberActions } from '#lib/server/members.ts';
import type { KitEvent } from '#lib/server/resource.ts';

export const load = (event: KitEvent) => loadMembers(event);
export const actions = memberActions;
