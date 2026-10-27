import { error } from '@sveltejs/kit';
import { GATEWAY_API_URL } from '$app/env/private';
import { API_BASE } from '#lib/server/api.ts';
import { requirePlatformAdmin } from '#lib/server/tester.ts';
import type { RequestHandler } from './$types';

/** The report byte for byte as it was sent: uploaded again, it is the same report. */
export const GET: RequestHandler = async ({ locals, params }) => {
	const token = requirePlatformAdmin(locals);
	const upstream = await fetch(
		`${GATEWAY_API_URL}${API_BASE}/benchmark-reports/${encodeURIComponent(params.id)}/document`,
		{ headers: { authorization: `Bearer ${token}` } }
	).catch(() => null);
	if (!upstream) error(502, 'The management API of the gateway did not answer.');
	if (!upstream.ok) error(upstream.status === 404 ? 404 : 502, 'The report could not be read.');
	return new Response(upstream.body, {
		headers: {
			'content-type': 'application/json',
			'content-disposition':
				upstream.headers.get('content-disposition') ?? `attachment; filename="${params.id}.json"`
		}
	});
};
