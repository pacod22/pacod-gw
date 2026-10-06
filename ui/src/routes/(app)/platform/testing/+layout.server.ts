import { requirePlatformAdmin, testerConfigured } from '#lib/server/tester.ts';
import type { LayoutServerLoad } from './$types';

/** Testing acts with the admin's session and creates objects: platform admins only. */
export const load: LayoutServerLoad = ({ locals }) => {
	requirePlatformAdmin(locals);
	return { testerConfigured: testerConfigured() };
};
