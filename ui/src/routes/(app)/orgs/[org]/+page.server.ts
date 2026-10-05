import { redirect } from '@sveltejs/kit';
import { organisationPath } from '#lib/nav.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = ({ params }) => {
	redirect(307, organisationPath(params.org));
};
