import type { ApiProblem } from '#lib/api/errors.ts';
import { problemOf } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const [status, changes] = await Promise.all([
		event.locals.api.GET('/system/status'),
		event.locals.api.GET('/system/config-versions', { params: { query: { limit: 25 } } })
	]);
	let problem: ApiProblem | null = null;
	if (!status.response.ok) problem = problemOf(event, status);
	else if (!changes.response.ok) problem = problemOf(event, changes);
	return {
		status: status.data ?? null,
		changes: changes.data ?? [],
		problem
	};
};
