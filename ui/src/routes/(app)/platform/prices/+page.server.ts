import { problemOf } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const result = await event.locals.api.GET('/prices');
	return {
		prices: result.data ?? [],
		problem: result.response.ok ? null : problemOf(event, result)
	};
};
