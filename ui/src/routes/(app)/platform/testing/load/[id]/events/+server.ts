import { error } from '@sveltejs/kit';
import { relayEvents, requirePlatformAdmin } from '#lib/server/tester.ts';
import { RUN_ID } from '#lib/testing.ts';
import type { RequestHandler } from './$types';

/** Passes a load test's seconds on to the browser as the tester adds them up. */
export const GET: RequestHandler = async ({ locals, params, request }) => {
	requirePlatformAdmin(locals);
	if (!RUN_ID.test(params.id)) error(404, 'There is no such load test.');
	return relayEvents(locals, `/load/runs/${params.id}/events`, request.signal);
};
