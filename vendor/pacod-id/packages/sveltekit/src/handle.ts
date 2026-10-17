// One SvelteKit Handle for pacod-id: the routes /auth/login, /auth/callback,
// /auth/logout and /auth/backchannel-logout, and on every other request the session, refreshed
// shortly before its access token expires, in `event.locals.session`.
//
// The routes are the server's, not pages: link to them with `data-sveltekit-reload` (or post a
// form to /auth/logout), or SvelteKit's client-side router tries to load them as pages.
//
// The back-channel logout is a form POST from pacod-id's server. SvelteKit refuses cross-site
// form posts before any Handle runs; pacod-id sends its issuer as the Origin, so add the issuer
// to `csrf.trustedOrigins` in the application's SvelteKit configuration.

import {
	IdClient,
	SignedOut,
	safeNext,
	type IdClientConfig,
	type IdSession,
	type LoginOptions,
	type PendingLogin
} from '@pacod/id-client';
import { keyOf, seal, unseal } from './seal.js';
import type { SessionStore } from './stores.js';
import type { CookieOptions, Cookies, RequestEvent, Resolve } from './types.js';

export interface IdHandleConfig extends IdClientConfig {
	/**
	 * The application's public origin (`https://console.example.com`). A request on another origin
	 * is sent there first, so the sign-in's cookie and its callback share one origin.
	 */
	origin: string;
	store: SessionStore;
	/** At least 32 characters: seals the sign-in's state cookie. */
	secret: string;
	/** Where the routes are; `/auth` by default. */
	basePath?: string;
	/** Refresh this many seconds before the access token expires; 60 by default. */
	refreshBefore?: number;
}

const PENDING = 'pid_login';

function redirect(to: string, headers: Headers = new Headers()): Response {
	headers.set('location', to);
	headers.set('cache-control', 'no-store');
	return new Response(null, { status: 303, headers });
}

/**
 * Cookies for the package's own routes. SvelteKit adds `event.cookies` changes only to responses
 * that come from `resolve`, so these routes write their `Set-Cookie` headers themselves.
 */
class RouteCookies implements Cookies {
	private changed = new Map<string, string | undefined>();
	private headers: string[] = [];

	constructor(private inner: Cookies) {}

	get(name: string): string | undefined {
		return this.changed.has(name) ? this.changed.get(name) : this.inner.get(name);
	}

	set(name: string, value: string, opts: CookieOptions): void {
		this.changed.set(name, value);
		this.headers.push(serialize(name, value, opts));
	}

	delete(name: string, opts: { path: string; secure?: boolean }): void {
		this.changed.set(name, undefined);
		this.headers.push(serialize(name, '', { ...opts, httpOnly: true, sameSite: 'lax', maxAge: 0 }));
	}

	/** The response with the cookies written. */
	apply(response: Response): Response {
		for (const h of this.headers) response.headers.append('set-cookie', h);
		return response;
	}
}

/** Values here are URL-safe already (sealed JWEs, random ids). */
function serialize(name: string, value: string, o: CookieOptions): string {
	const parts = [`${name}=${value}`, `Path=${o.path}`];
	if (o.maxAge !== undefined) parts.push(`Max-Age=${o.maxAge}`);
	if (o.httpOnly) parts.push('HttpOnly');
	if (o.secure) parts.push('Secure');
	if (o.sameSite) parts.push(`SameSite=${o.sameSite[0].toUpperCase()}${o.sameSite.slice(1)}`);
	return parts.join('; ');
}

/** Runs one of the package's routes with cookies it writes itself. */
async function route(
	event: RequestEvent,
	fn: (event: RequestEvent) => Promise<Response>
): Promise<Response> {
	const cookies = new RouteCookies(event.cookies);
	const response = await fn({ ...event, cookies });
	return cookies.apply(response);
}

/** The session of this request, refreshed when due. Also in `event.locals.session`. */
export type IdLocals = { session: IdSession | null };

export function createIdHandle(config: IdHandleConfig) {
	const client = new IdClient(config);
	const base = (config.basePath ?? '/auth').replace(/\/$/, '');
	const secure = config.origin.startsWith('https:');
	const origin = new URL(config.origin).origin;
	const key = keyOf(config.secret, 'pending-login');
	const refreshBefore = config.refreshBefore ?? 60;
	const store = config.store;

	async function login(event: RequestEvent): Promise<Response> {
		const q = event.url.searchParams;
		const opts: LoginOptions = {
			next: q.get('next') ?? '/',
			prompt: (['login', 'create', 'none'] as const).find((p) => p === q.get('prompt')),
			uiLocales: q.get('ui_locales') ?? undefined,
			loginHint: q.get('login_hint') ?? undefined
		};
		const { url, pending } = await client.startLogin(opts);
		event.cookies.set(PENDING, await seal(pending, await key), {
			path: base,
			httpOnly: true,
			secure,
			// Lax: the cookie comes along on the top-level redirect back from pacod-id.
			sameSite: 'lax',
			maxAge: 600
		});
		return redirect(url);
	}

	async function callback(event: RequestEvent): Promise<Response> {
		const pending = await unseal<PendingLogin>(event.cookies.get(PENDING), await key);
		event.cookies.delete(PENDING, { path: base, secure });
		if (!pending)
			return new Response('The sign-in expired or was started in another browser. Try again.', {
				status: 400
			});
		if (event.url.searchParams.get('error')) {
			// prompt=none without a session, or the person cancelled.
			return redirect(safeNext(pending.next));
		}
		const session = await client.finishLogin(event.url, pending);
		await store.save(event.cookies, session, secure, true);
		return redirect(safeNext(pending.next));
	}

	async function logout(event: RequestEvent): Promise<Response> {
		const session = await store.load(event.cookies, secure);
		const to = await client.signOut(session);
		await store.destroy(event.cookies, session, secure);
		return redirect(config.postLogoutRedirectUri ? to : '/');
	}

	async function backchannel(event: RequestEvent): Promise<Response> {
		const headers = { 'cache-control': 'no-store', 'content-type': 'application/json' };
		try {
			const form = await event.request.formData();
			const { sid } = await client.verifyLogoutToken(String(form.get('logout_token') ?? ''));
			await store.endSid(sid);
			return new Response('{}', { status: 200, headers });
		} catch {
			return new Response('{"error":"invalid_request"}', { status: 400, headers });
		}
	}

	/** The session, with fresh tokens; `null` once pacod-id has ended it. */
	async function current(event: RequestEvent): Promise<IdSession | null> {
		let session = await store.load(event.cookies, secure);
		if (!session) return null;
		if (await store.ended(session)) {
			await store.destroy(event.cookies, session, secure);
			return null;
		}
		const due = (s: IdSession) => s.accessExpiresAt - Math.floor(Date.now() / 1000) < refreshBefore;
		if (!due(session)) return session;
		const run = async () => {
			// Another instance may have refreshed meanwhile (a store with server state).
			const latest = (await store.load(event.cookies, secure)) ?? session!;
			if (!due(latest)) return latest;
			const next = await client.refresh(latest);
			await store.save(event.cookies, next, secure);
			return next;
		};
		try {
			session = store.locked ? await store.locked(session, run) : await run();
		} catch (e) {
			if (e instanceof SignedOut) {
				await store.destroy(event.cookies, session, secure);
				return null;
			}
			// pacod-id cannot be reached: the access token serves while it lasts.
			console.warn(`pacod-id: refreshing a session failed: ${(e as Error).message}`);
			if (session.accessExpiresAt <= Math.floor(Date.now() / 1000)) return null;
		}
		return session;
	}

	const handle = async ({
		event,
		resolve
	}: {
		event: RequestEvent;
		resolve: Resolve;
	}): Promise<Response> => {
		const path = event.url.pathname;
		if (
			event.url.origin !== origin &&
			(event.request.method === 'GET' || event.request.method === 'HEAD')
		) {
			return redirect(`${origin}${path}${event.url.search}`);
		}
		if (path === `${base}/login` && event.request.method === 'GET') return route(event, login);
		if (path === `${base}/callback` && event.request.method === 'GET')
			return route(event, callback);
		if (path === `${base}/logout`) {
			return event.request.method === 'POST'
				? route(event, logout)
				: new Response(null, { status: 405, headers: { allow: 'POST' } });
		}
		if (path === `${base}/backchannel-logout` && event.request.method === 'POST')
			return backchannel(event);
		(event.locals as IdLocals).session = await current(event);
		return (resolve as (e: RequestEvent) => Response | Promise<Response>)(event);
	};

	/** Where to send the browser to sign in, then back to `next`. */
	const loginPath = (next = '/') => `${base}/login?next=${encodeURIComponent(safeNext(next))}`;

	return Object.assign(handle, { client, loginPath });
}
