import { error } from '@sveltejs/kit';
import type { ApiProblem } from '#lib/api/errors.ts';
import { isUsageEvent, type UsageEvent } from '#lib/insight/usage.ts';
import { problemOf } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const result = await event.locals.api.GET('/requests/{id}', {
		params: { path: { id: event.params.id } }
	});

	let request: UsageEvent | null = null;
	let problem: ApiProblem | null = null;
	if (result.response.ok && isUsageEvent(result.data)) {
		request = result.data;
	} else if (result.response.ok) {
		error(502, 'The gateway answered with something that is not a request record.');
	} else {
		problem = problemOf(event, result);
		if (problem.status === 404) {
			error(
				404,
				'This request is not in the log. It may be older than the retention period, or belong to a workspace you may not see.'
			);
		}
	}
	return { request, problem };
};
