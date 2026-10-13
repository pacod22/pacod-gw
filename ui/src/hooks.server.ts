import { redirect } from '@sveltejs/kit';
import { sequence, type Handle } from '@sveltejs/kit/hooks';
import { GATEWAY_API_URL } from '$app/env/private';
import { toProblem } from '#lib/api/errors.ts';
import { AUTH_BASE, identityRedirect, signInPath } from '#lib/identity.ts';
import { createApi } from '#lib/server/api.ts';
import { identity } from '#lib/server/identity.ts';
import { clearSession, loginPath, returnTarget, SESSION_COOKIE } from '#lib/server/session.ts';

/** Pages that are shown without a session. */
const PUBLIC = new Set([
	'/login',
	'/logout',
	'/forgot-password',
	'/reset-password',
	'/verify-email',
	'/invite'
]);

const app: Handle = async ({ event, resolve }) => {
	const { locals, cookies, url } = event;
	if (identity) {
		// The pages of the console's own login lead to pacod-id's.
		const to = identityRedirect(identity.issuer, url, event.request.method);
		if (to) {
			// 307 keeps the sign-out form's POST.
			const status = to === `${AUTH_BASE}/logout` ? 307 : 303;
			return new Response(null, { status, headers: { location: to, 'cache-control': 'no-store' } });
		}
		locals.token = locals.session?.accessToken ?? null;
	} else {
		locals.token = cookies.get(SESSION_COOKIE) ?? null;
	}
	locals.api = createApi(GATEWAY_API_URL, locals.token);
	let ended = false;
	locals.principal = null;
	locals.apiProblem = null;

	if (locals.token) {
		// Who is calling: asked once per request, here.
		const { data, error, response } = await locals.api.GET('/session');
		if (response.ok && data) {
			locals.principal = data;
		} else if (response.status === 401) {
			// The session has ended or was revoked: forget the token.
			if (identity) identity.forget(cookies);
			else clearSession(cookies);
			ended = true;
			locals.token = null;
			locals.api = createApi(GATEWAY_API_URL, null);
		} else {
			// The core is down or failing. The session may still be good, so keep the cookie
			// and let the page explain.
			locals.apiProblem = toProblem(response.status, error);
		}
	}

	if (!locals.principal && !PUBLIC.has(url.pathname)) {
		if (url.pathname === '/events') {
			return new Response(locals.apiProblem ? 'The gateway does not answer.' : 'Sign in first.', {
				status: locals.apiProblem ? 502 : 401
			});
		}
		// With `apiProblem` set, the layout of the console answers with an error page.
		if (!locals.apiProblem) {
			redirect(303, identity ? signInPath(returnTarget(url), ended) : loginPath(url));
		}
	}

	const response = await resolve(event);
	// Pages hold configuration and, once, secrets: nothing may keep a copy.
	if (!response.headers.has('cache-control')) response.headers.set('cache-control', 'no-store');
	response.headers.set('x-content-type-options', 'nosniff');
	response.headers.set('x-frame-options', 'DENY');
	response.headers.set('referrer-policy', 'same-origin');
	return response;
};

/** With pacod-id, its sign-in routes and the session come first (`lib/server/identity.ts`). */
export const handle: Handle = identity ? sequence(identity.handle as unknown as Handle, app) : app;
