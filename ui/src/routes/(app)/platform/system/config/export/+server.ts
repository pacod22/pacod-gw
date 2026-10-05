import { error } from '@sveltejs/kit';
import type { RequestHandler } from './$types';

/** The configuration of the installation as a file to keep or to import elsewhere. */
export const GET: RequestHandler = async ({ locals }) => {
	const result = await locals.api.GET('/system/config-export');
	if (!result.response.ok || !result.data) {
		error(result.response.status === 403 ? 403 : 502, 'The configuration could not be exported.');
	}
	const stamp = result.data.exported_at.slice(0, 19).replace(/[:T]/g, '-');
	return new Response(JSON.stringify(result.data, null, 2), {
		headers: {
			'content-type': 'application/json',
			'content-disposition': `attachment; filename="pacod-gw-config-v${result.data.config_version}-${stamp}.json"`
		}
	});
};
