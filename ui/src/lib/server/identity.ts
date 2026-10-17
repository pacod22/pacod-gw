/**
 * Sign-in through pacod-id, with PGW_AUTH=oidc. The console is a
 * confidential client of pacod-id (a BFF): `@pacod/id-sveltekit` serves /auth/login,
 * /auth/callback and /auth/logout, and keeps pacod-id's tokens sealed in one cookie;
 * `locals.token` is the session's access token, refreshed shortly before it expires. With
 * PGW_AUTH=local (the default) the console keeps its own login against the core.
 *
 * The console takes no back-channel logout: a session ended at pacod-id is refused by the core
 * within seconds (its copy of pacod-id's feed), and the console then forgets the cookie.
 */
import {
	PGW_AUTH,
	PGW_CONSOLE_URL,
	PGW_ID_INTERNAL_URL,
	PGW_ID_ISSUER,
	PGW_UI_CLIENT_ID,
	PGW_UI_CLIENT_SECRET,
	PGW_UI_SESSION_SECRET
} from '$app/env/private';
import type { Cookies } from '@sveltejs/kit';
import { createIdHandle, sealedCookie } from '@pacod/id-sveltekit';

/** The cookie's name; `__Host-` is put in front of it on https. */
const COOKIE = 'pgw_session';

function required(name: string, value: string | undefined): string {
	if (!value)
		throw new Error(`${name} is required with PGW_AUTH=oidc (signing in through pacod-id)`);
	return value;
}

function start(issuer: string) {
	const origin = required('PGW_CONSOLE_URL', PGW_CONSOLE_URL);
	const sessionSecret = required('PGW_UI_SESSION_SECRET', PGW_UI_SESSION_SECRET);
	if (sessionSecret.length < 32)
		throw new Error('PGW_UI_SESSION_SECRET must have at least 32 characters');
	const store = sealedCookie({ name: COOKIE, secret: sessionSecret });
	const handle = createIdHandle({
		issuer,
		internalUrl: PGW_ID_INTERNAL_URL ?? issuer,
		clientId: PGW_UI_CLIENT_ID,
		clientSecret: required('PGW_UI_CLIENT_SECRET', PGW_UI_CLIENT_SECRET),
		redirectUri: `${origin}/auth/callback`,
		postLogoutRedirectUri: `${origin}/`,
		origin,
		store,
		secret: sessionSecret
	});
	const secure = origin.startsWith('https:');
	return {
		issuer,
		handle,
		/** Forgets the session in this browser (the core refused its token), as the store would. */
		forget(cookies: Cookies): void {
			cookies.delete(secure ? `__Host-${COOKIE}` : COOKIE, { path: '/', secure });
		}
	};
}

/** Signing in through pacod-id, or `null` when the console keeps its own login. */
export const identity =
	PGW_AUTH === 'oidc' ? start(required('PGW_ID_ISSUER', PGW_ID_ISSUER)) : null;
