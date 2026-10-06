import { redirect } from '@sveltejs/kit';
import { BOARDS } from '#lib/metrics/boards.ts';
import type { PageServerLoad } from './$types';

/** The first board, with the filters of the address. */
export const load: PageServerLoad = ({ url }) => {
	redirect(307, `/insight/metrics/${BOARDS[0].id}${url.search}`);
};
