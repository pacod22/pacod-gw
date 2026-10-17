/**
 * Where the console sends people when pacod-id signs them in: the pages
 * of its own login become pacod-id's, and old links lead on. Pure, so that it is tested without
 * SvelteKit; `lib/server/identity.ts` applies it.
 */

/** The console's own sign-in routes, served by `@pacod/id-sveltekit`. */
export const AUTH_BASE = '/auth';

/** Only paths of this console are accepted as the place to go after signing in. */
export function safeNext(next: string | null | undefined): string {
	if (!next || !next.startsWith('/') || next.startsWith('//') || next.includes('\\')) return '/';
	if (next === '/login' || next.startsWith('/login?') || next.startsWith('/logout')) return '/';
	if (next === AUTH_BASE || next.startsWith(`${AUTH_BASE}/`)) return '/';
	return next;
}

/**
 * Signing in, then back to `next`. After a session ended (`ended`), pacod-id asks for the
 * password again instead of signing in silently, so that a refusal cannot loop.
 */
export function signInPath(next: string, ended = false): string {
	const query = new URLSearchParams({ next: safeNext(next) });
	if (ended) query.set('prompt', 'login');
	return `${AUTH_BASE}/login?${query}`;
}

/** Where a page of the console's own login leads with pacod-id, or `null` for any other path. */
export function identityRedirect(issuer: string, url: URL, method: string): string | null {
	switch (url.pathname) {
		case '/login':
			return signInPath(url.searchParams.get('next') ?? '/');
		case '/forgot-password':
			return `${issuer}/forgot-password`;
		// The console's own links cannot work at pacod-id: a new one is asked for there.
		case '/reset-password':
			return `${issuer}/forgot-password`;
		case '/verify-email':
			return `${issuer}/account`;
		case '/invite': {
			// pacod-id took over the invitations with their links.
			const token = url.searchParams.get('token');
			return token
				? `${issuer}/invite?${new URLSearchParams({ token })}`
				: `${issuer}/account/organisations`;
		}
		case '/account/password':
			return `${issuer}/account/security`;
		case '/logout':
			// The shell's form posts here; the sign-out itself is the package's.
			return method === 'POST' ? `${AUTH_BASE}/logout` : '/';
		default:
			return null;
	}
}

/** pacod-id's account page, where people manage their sign-in and their invitations. */
export const accountUrl = (issuer: string) => `${issuer}/account`;
export const invitationsUrl = (issuer: string) => `${issuer}/account/organisations`;
