import { error } from '@sveltejs/kit';
import type { RequestHandler } from './$types';

/** The template as a file, to import into a workspace, an organisation or another installation. */
export const GET: RequestHandler = async ({ locals, params }) => {
	const result = await locals.api.GET('/orgs/{org}/templates/{id}/export', {
		params: { path: { org: params.org, id: params.id } }
	});
	if (!result.response.ok || !result.data)
		error(result.response.status, 'The template could not be exported.');
	return new Response(JSON.stringify(result.data, null, 2), {
		headers: {
			'content-type': 'application/json',
			'content-disposition': `attachment; filename="${result.data.name}.template.json"`
		}
	});
};
