/** The login session: one httpOnly cookie that holds the core's session token. */
import { redirect, type Cookies } from '@sveltejs/kit';

export const SESSION_COOKIE = 'pgw_session';

export function setSession(cookies: Cookies, token: string, expiresAt: string): void {
	const expires = new Date(expiresAt);
	cookies.set(SESSION_COOKIE, token, {
		path: '/',
		httpOnly: true,
		sameSite: 'lax',
		// Without a valid time the cookie ends with the browser session.
		...(Number.isNaN(expires.getTime()) ? {} : { expires })
	});
}

export function clearSession(cookies: Cookies): void {
	cookies.delete(SESSION_COOKIE, { path: '/' });
}

/** Only paths of this console are accepted as the place to go after signing in. */
export function safeNext(next: string | null | undefined): string {
	if (!next || !next.startsWith('/') || next.startsWith('//') || next.includes('\\')) return '/';
	if (next === '/login' || next.startsWith('/login?') || next.startsWith('/logout')) return '/';
	return next;
}

/** The login page, remembering where the user was going. */
export function loginPath(url: URL): string {
	// A form action is addressed as `?/name`: return to the page, not to the action.
	const query = new URLSearchParams([...url.searchParams].filter(([key]) => !key.startsWith('/')));
	const search = query.toString();
	const target = url.pathname + (search ? `?${search}` : '');
	return target === '/' ? '/login' : `/login?next=${encodeURIComponent(target)}`;
}

/** The core no longer accepts the session: forget it and ask the user to sign in again. */
export function endSession(event: { cookies: Cookies; url: URL }): never {
	clearSession(event.cookies);
	redirect(303, loginPath(event.url));
}
