import { error } from '@sveltejs/kit';
import { relayEvents, requirePlatformAdmin } from '#lib/server/tester.ts';
import { RUN_ID } from '#lib/testing.ts';
import type { RequestHandler } from './$types';

/** Passes the tester's progress events of a run on to the browser. */
export const GET: RequestHandler = async ({ locals, params, request }) => {
	requirePlatformAdmin(locals);
	if (!RUN_ID.test(params.id)) error(404, 'There is no such run.');
	return relayEvents(locals, `/runs/${params.id}/events`, request.signal);
};
