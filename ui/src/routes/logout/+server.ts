import { redirect } from '@sveltejs/kit';
import { clearSession, safeNext } from '#lib/server/session.ts';
import type { RequestHandler } from './$types';

/** Ends the session at the core and forgets the cookie. Reached by the form in the shell. */
export const POST: RequestHandler = async ({ locals, cookies, request }) => {
	if (locals.token) {
		// Even when the core cannot be told, the browser forgets the token.
		await locals.api.DELETE('/session');
	}
	clearSession(cookies);
	// A page that signs someone out to go on, such as an invitation for another account.
	const next = (await request.formData().catch(() => null))?.get('next');
	if (typeof next === 'string' && next) redirect(303, safeNext(next));
	redirect(303, '/login?reason=signed-out');
};

/** Following a link here does nothing but lead on. */
export const GET: RequestHandler = () => {
	redirect(303, '/');
};
