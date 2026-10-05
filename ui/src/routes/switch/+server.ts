import { redirect } from '@sveltejs/kit';
import { switchTarget } from '#lib/nav.ts';
import type { RequestHandler } from './$types';

/**
 * The organisation and workspace switcher without JavaScript: its form is sent here and
 * answered with a redirect to the page for the chosen scope.
 */
export const GET: RequestHandler = ({ url }) => {
	const value = (name: string) => url.searchParams.get(name) ?? '';
	redirect(
		303,
		switchTarget({ org: value('org'), ws: value('ws'), was: value('was'), from: value('from') })
	);
};
