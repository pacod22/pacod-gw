import { error } from '@sveltejs/kit';
import { GATEWAY_API_URL } from '$app/env/private';
import { toProblem } from '#lib/api/errors.ts';
import { API_BASE } from '#lib/server/api.ts';
import type { RequestHandler } from './$types';

/**
 * The calls of one trace or one session as JSON lines, oldest first. With `audit=true` each
 * line also holds the call's audit record; the gateway gives that only to who may read audit
 * records, and writes every export to its audit log.
 */
export const GET: RequestHandler = async (event) => {
	if (!event.locals.token) error(401, 'Sign in first.');
	const search = new URLSearchParams();
	for (const name of ['trace_id', 'session_id', 'audit', 'organisation_id', 'workspace_id']) {
		const value = event.url.searchParams.get(name);
		if (value) search.set(name, value);
	}
	let upstream: Response;
	try {
		upstream = await fetch(`${GATEWAY_API_URL}${API_BASE}/requests/export?${search}`, {
			headers: { authorization: `Bearer ${event.locals.token}` },
			signal: event.request.signal
		});
	} catch {
		error(502, 'The gateway does not answer.');
	}
	if (!upstream.ok) {
		const problem = toProblem(upstream.status, await upstream.json().catch(() => null));
		error(
			upstream.status >= 400 && upstream.status <= 599 ? upstream.status : 502,
			problem.message
		);
	}
	const which = search.get('trace_id')
		? `trace-${search.get('trace_id')}`
		: `session-${search.get('session_id')}`;
	const file = `pacod-gw-${which}.jsonl`.replace(/[^A-Za-z0-9._-]+/g, '-');
	return new Response(upstream.body, {
		headers: {
			'content-type': 'application/x-ndjson',
			'content-disposition': `attachment; filename="${file}"`
		}
	});
};
