import { redirect } from '@sveltejs/kit';
import { workspacePath } from '#lib/nav.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = ({ params }) => {
	redirect(307, workspacePath(params.org, params.ws));
};
