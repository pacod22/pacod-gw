import { error } from '@sveltejs/kit';
import { requirePlatformAdmin, tester } from '#lib/server/tester.ts';
import type { RequestHandler } from './$types';

/** A suite's file, built-in or uploaded: to read, or to start a suite of one's own from. */
export const GET: RequestHandler = async ({ locals, params }) => {
	requirePlatformAdmin(locals);
	if (!/^[a-z0-9][a-z0-9-]{0,62}$/.test(params.suite)) error(404, 'There is no such suite.');
	const result = await tester<string>(locals, 'GET', `/cases/${params.suite}`);
	if (result.problem) error(result.problem.status === 404 ? 404 : 502, result.problem.message);
	return new Response(result.data, {
		headers: {
			'content-type': 'text/yaml; charset=utf-8',
			'content-disposition': `inline; filename="${params.suite}.yaml"`
		}
	});
};
