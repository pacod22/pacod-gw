import { problemOf } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	// The system status is for platform admins; everyone else gets an orientation.
	if (!event.locals.principal?.platform_admin) return { status: null, problem: null };
	const result = await event.locals.api.GET('/system/status');
	if (result.response.ok && result.data) return { status: result.data, problem: null };
	return { status: null, problem: problemOf(event, result) };
};
