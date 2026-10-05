import { loose } from '#lib/server/api.ts';
import { problemOf } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

// Reading an audit record is itself logged, so it is loaded here, when the tab is opened,
// and not together with the call.
export const load: PageServerLoad = async (event) => {
	const result = await loose(event.locals.api).GET('/requests/{id}/audit', {
		params: { path: { id: event.params.id } }
	});
	if (result.response.ok)
		return { record: result.data as Record<string, unknown>, notAudited: false, problem: null };
	const body = result.error as { error?: { details?: { code?: string } } } | undefined;
	if (result.response.status === 404 && body?.error?.details?.code === 'not_audited') {
		return { record: null, notAudited: true, problem: null };
	}
	return { record: null, notAudited: false, problem: problemOf(event, result) };
};
