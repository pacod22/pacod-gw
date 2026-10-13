import { describe, expect, it } from 'vitest';
import type { IdSession } from '@pacod/id-client';
import { createIdHandle } from './handle.js';
import { memoryStore, redisStore, sealedCookie, type RedisLike } from './stores.js';
import type { CookieOptions, Cookies, RequestEvent } from './types.js';

function jar(): Cookies & { all: Map<string, { value: string; opts: CookieOptions }> } {
	const all = new Map<string, { value: string; opts: CookieOptions }>();
	return {
		all,
		get: (n) => all.get(n)?.value,
		set: (n, value, opts) => void all.set(n, { value, opts }),
		delete: (n) => void all.delete(n)
	};
}

const now = () => Math.floor(Date.now() / 1000);
// About the size of a real RS256 access token of pacod-id.
const accessToken = `eyJ${'x'.repeat(1100)}.${'y'.repeat(342)}`;
const session = (sid = 'sid_1'): IdSession => ({
	sub: 'usr_01a1311e1f5d3c7a9b4e2f6c8d0a1b2c',
	sid,
	accessToken,
	accessExpiresAt: now() + 300,
	refreshToken: `pidr_01a1311e1f5d3c7a9b4e2f6c8d0a1b2c_${'r'.repeat(43)}`,
	name: 'Ada Lovelace',
	email: 'ada.lovelace@example.com',
	createdAt: now()
});

describe('stores', () => {
	it('a sealed cookie keeps a session under 4 KB, unreadable without the secret', async () => {
		const store = sealedCookie({ name: 's', secret: 'a-secret-of-at-least-thirty-two-chars' });
		const c = jar();
		await store.save(c, session(), true);
		const raw = c.all.get('__Host-s')!;
		expect(raw.value.length).toBeLessThan(4000);
		expect(raw.opts).toMatchObject({ httpOnly: true, secure: true, sameSite: 'lax', path: '/' });
		// Neither the cookie nor any part of it read as base64url shows the name (a signed but
		// unencrypted token would). The whole name, with its space, never turns up by chance.
		const decoded = raw.value.split('.').map((p) => Buffer.from(p, 'base64url').toString('latin1'));
		for (const text of [raw.value, ...decoded]) expect(text).not.toContain('Ada Lovelace');
		expect((await store.load(c, true))?.sid).toBe('sid_1');
		expect(await store.load(c, false), 'over https only the __Host- cookie counts').toBeNull();
		const other = sealedCookie({ name: 's', secret: 'another-secret-of-at-least-32-chars!' });
		expect(await other.load(c, true)).toBeNull();
		await store.endSid('sid_1');
		expect(await store.ended(session())).toBe(true);
	});

	it('memory sessions end by sid', async () => {
		const store = memoryStore();
		const c = jar();
		await store.save(c, session('sid_9'), false);
		expect(store.size()).toBe(1);
		await store.endSid('sid_9');
		expect(await store.load(c, false)).toBeNull();
	});

	it('a sign-in never keeps a session id the browser brought', async () => {
		const store = memoryStore();
		const c = jar();
		c.set('pid_app_session', 'planted-by-someone-else', { path: '/' });
		await store.save(c, session('sid_new'), false, true);
		expect(c.get('pid_app_session')).not.toBe('planted-by-someone-else');
		const before = c.get('pid_app_session');
		await store.save(c, session('sid_new'), false, true);
		expect(c.get('pid_app_session')).not.toBe(before);
		expect(store.size(), 'the old one went').toBe(1);
	});

	it('Redis sessions end by sid and lock across instances', async () => {
		const data = new Map<string, string>();
		const client: RedisLike = {
			get: async (k) => data.get(k) ?? null,
			set: async (k, v, o) => {
				if (o.nx && data.has(k)) return false;
				data.set(k, v);
				return true;
			},
			del: async (k) => void data.delete(k)
		};
		const store = redisStore({ client });
		const c = jar();
		c.set('pid_app_session', 'planted', { path: '/' });
		await store.save(c, session('sid_r'), false, true);
		expect(c.get('pid_app_session')).not.toBe('planted');
		const loaded = (await store.load(c, false))!;
		let inside = 0;
		const both = await Promise.all([
			store.locked!(loaded, async () => {
				inside++;
				await new Promise((r) => setTimeout(r, 50));
				return inside;
			}),
			store.locked!(loaded, async () => ++inside)
		]);
		expect(both.sort()).toEqual([1, 2]);
		await store.endSid('sid_r');
		expect(await store.load(c, false)).toBeNull();
	});
});

describe('the handle', () => {
	const handle = createIdHandle({
		issuer: 'http://id.example.test',
		clientId: 'app',
		clientSecret: 'a-client-secret-for-the-unit-tests-0123',
		redirectUri: 'http://app.example.test/auth/callback',
		origin: 'http://app.example.test',
		store: memoryStore(),
		secret: 'a-secret-of-at-least-thirty-two-chars'
	});
	const event = (url: string, init?: RequestInit): RequestEvent => ({
		url: new URL(url),
		request: new Request(url, init),
		cookies: jar(),
		locals: {}
	});
	const resolve = () => new Response('page');

	it('sends another origin to its own, and starts a sign-in with PKCE and a sealed state', async () => {
		const r = await handle({ event: event('http://127.0.0.1:3000/orgs?x=1'), resolve });
		expect(r.headers.get('location')).toBe('http://app.example.test/orgs?x=1');
		const e = event('http://app.example.test/auth/login?next=/orgs');
		const login = await handle({ event: e, resolve });
		const to = new URL(login.headers.get('location')!);
		expect(to.origin + to.pathname).toBe('http://id.example.test/oidc/authorize');
		expect(to.searchParams.get('code_challenge_method')).toBe('S256');
		// Written on the response itself: SvelteKit adds event.cookies only to resolved responses.
		const cookie = login.headers.get('set-cookie')!;
		expect(cookie).toMatch(/^pid_login=[^;]+; Path=\/auth; Max-Age=600; HttpOnly; SameSite=Lax$/);
		expect(cookie).not.toContain(to.searchParams.get('state')!);
		expect(
			(e.cookies as ReturnType<typeof jar>).all.has('pid_login'),
			'not in the event cookies'
		).toBe(false);
	});

	it('refuses a bad back-channel logout and a sign-out by GET', async () => {
		const r = await handle({
			event: event('http://app.example.test/auth/backchannel-logout', {
				method: 'POST',
				body: new URLSearchParams({ logout_token: 'nonsense' })
			}),
			resolve
		});
		expect(r.status).toBe(400);
		expect(
			(await handle({ event: event('http://app.example.test/auth/logout'), resolve })).status
		).toBe(405);
	});

	it('passes other requests on with no session', async () => {
		const e = event('http://app.example.test/');
		expect(await (await handle({ event: e, resolve })).text()).toBe('page');
		expect((e.locals as { session: unknown }).session).toBeNull();
	});
});
