// Where sessions live: a sealed cookie (stateless; the gateway console), Redis
// (shop web and bazaar web), memory (tests and development).

import type { IdSession } from '@pacod/id-client';
import { keyOf, seal, unseal } from './seal.js';
import type { CookieOptions, Cookies } from './types.js';

export interface SessionStore {
	/** The session this request carries. */
	load(cookies: Cookies, secure: boolean): Promise<IdSession | null>;
	/**
	 * Keeps the session. `fresh`: a new sign-in, which gets a new id, so that a session id
	 * planted in the browser beforehand is never the signed-in one.
	 */
	save(cookies: Cookies, session: IdSession, secure: boolean, fresh?: boolean): Promise<void>;
	destroy(cookies: Cookies, session: IdSession | null, secure: boolean): Promise<void>;
	/** A back-channel logout: session `sid` has ended at pacod-id. */
	endSid(sid: string): Promise<void>;
	/** Whether a session has ended (a store without server state remembers recent ones). */
	ended(session: IdSession): Promise<boolean>;
	/**
	 * Runs `fn` with this session locked across instances, when the store can (Redis). A store
	 * without it relies on the process's own lock and pacod-id's 30-second grace.
	 */
	locked?<T>(session: IdSession, fn: () => Promise<T>): Promise<T>;
}

/**
 * The cookie's name: with `__Host-` over https, so that neither a sibling subdomain nor plain
 * http can set it, and only that name is read there.
 */
const cookieName = (name: string, secure: boolean) => (secure ? `__Host-${name}` : name);

/** What a store reads back, if it has a session's shape (a cookie can carry anything). */
function asSession(v: unknown): IdSession | null {
	const s = v as Partial<IdSession> | null;
	return s &&
		typeof s.sub === 'string' &&
		typeof s.sid === 'string' &&
		typeof s.accessToken === 'string' &&
		typeof s.accessExpiresAt === 'number'
		? (s as IdSession)
		: null;
}

const cookie = (secure: boolean, maxAge?: number): CookieOptions => ({
	path: '/',
	httpOnly: true,
	secure,
	sameSite: 'lax',
	maxAge
});

/** How long a session is kept: until its refresh token is no use (at most a day without one). */
function lifetime(session: IdSession): number {
	return session.refreshToken
		? 30 * 24 * 3600
		: Math.max(60, session.accessExpiresAt - Math.floor(Date.now() / 1000));
}

/** Ended sessions a stateless store remembers, by sid, until no access token can outlive them. */
class EndedSids {
	private sids = new Map<string, number>();
	add(sid: string) {
		this.prune();
		this.sids.set(sid, Date.now() + 24 * 3600_000);
	}
	has(sid: string) {
		this.prune();
		return this.sids.has(sid);
	}
	private prune() {
		const now = Date.now();
		for (const [k, until] of this.sids) if (until <= now) this.sids.delete(k);
		if (this.sids.size > 100_000) this.sids.clear();
	}
}

/**
 * The whole session in an AES-256-GCM sealed cookie (JWE `dir`/`A256GCM`): no server state. A
 * back-channel logout is remembered by this process; the product's API refuses the ended session
 * anyway, and the next refresh fails.
 */
export function sealedCookie(opts: { name: string; secret: string }): SessionStore {
	const k = keyOf(opts.secret, 'session');
	const ended = new EndedSids();
	return {
		async load(cookies, secure) {
			return asSession(await unseal(cookies.get(cookieName(opts.name, secure)), await k));
		},
		async save(cookies, session, secure) {
			const sealed = await seal(session, await k);
			if (sealed.length > 4000)
				throw new Error(`the sealed session is ${sealed.length} bytes; browsers keep 4096`);
			cookies.set(cookieName(opts.name, secure), sealed, cookie(secure, lifetime(session)));
		},
		async destroy(cookies, _session, secure) {
			cookies.delete(cookieName(opts.name, secure), { path: '/', secure });
		},
		async endSid(sid) {
			ended.add(sid);
		},
		async ended(session) {
			return ended.has(session.sid);
		}
	};
}

function newId(): string {
	const b = crypto.getRandomValues(new Uint8Array(24));
	return Buffer.from(b).toString('base64url');
}

/** Sessions in this process's memory; the cookie holds an id. For tests and development. */
export function memoryStore(opts: { name?: string } = {}): SessionStore & { size(): number } {
	const name = opts.name ?? 'pid_app_session';
	const sessions = new Map<string, IdSession>();
	return {
		size: () => sessions.size,
		async load(cookies, secure) {
			const id = cookies.get(cookieName(name, secure));
			return (id && sessions.get(id)) || null;
		},
		async save(cookies, session, secure, fresh) {
			const old = cookies.get(cookieName(name, secure));
			if (fresh && old) sessions.delete(old);
			const id = (!fresh && old) || newId();
			sessions.set(id, session);
			cookies.set(cookieName(name, secure), id, cookie(secure, lifetime(session)));
		},
		async destroy(cookies, _session, secure) {
			const id = cookies.get(cookieName(name, secure));
			if (id) sessions.delete(id);
			cookies.delete(cookieName(name, secure), { path: '/', secure });
		},
		async endSid(sid) {
			for (const [id, s] of sessions) if (s.sid === sid) sessions.delete(id);
		},
		async ended() {
			return false;
		}
	};
}

/** The few Redis commands the store needs; wrap ioredis or node-redis in it. */
export interface RedisLike {
	get(key: string): Promise<string | null>;
	/** SET with PX (milliseconds) and NX; whether it was set. */
	set(key: string, value: string, opts: { px: number; nx?: boolean }): Promise<boolean>;
	del(key: string): Promise<void>;
}

/**
 * Sessions in Redis: `<prefix>:<id>` (the session), `<prefix>_sid:<sid>` (its id, for back-channel
 * logout) and `<prefix>_lock:<id>` (one refresh at a time across instances).
 */
export function redisStore(opts: {
	client: RedisLike;
	name?: string;
	prefix?: string;
}): SessionStore {
	const name = opts.name ?? 'pid_app_session';
	const prefix = opts.prefix ?? 'app_session';
	const r = opts.client;
	const idOf = new WeakMap<IdSession, string>();
	return {
		async load(cookies, secure) {
			const id = cookies.get(cookieName(name, secure));
			if (!id) return null;
			const raw = await r.get(`${prefix}:${id}`);
			if (!raw) return null;
			const s = asSession(JSON.parse(raw));
			if (s) idOf.set(s, id);
			return s;
		},
		async save(cookies, session, secure, fresh) {
			const old = cookies.get(cookieName(name, secure));
			if (fresh && old) {
				// Whatever the browser carried, and its index, go: the sign-in gets its own id.
				const before = await r.get(`${prefix}:${old}`);
				await r.del(`${prefix}:${old}`);
				const prev = before ? asSession(JSON.parse(before)) : null;
				if (prev) await r.del(`${prefix}_sid:${prev.sid}`);
			}
			const id = (!fresh && old) || newId();
			const ms = lifetime(session) * 1000;
			await r.set(`${prefix}:${id}`, JSON.stringify(session), { px: ms });
			await r.set(`${prefix}_sid:${session.sid}`, id, { px: ms });
			idOf.set(session, id);
			cookies.set(cookieName(name, secure), id, cookie(secure, lifetime(session)));
		},
		async destroy(cookies, session, secure) {
			const id = cookies.get(cookieName(name, secure));
			if (id) await r.del(`${prefix}:${id}`);
			if (session) await r.del(`${prefix}_sid:${session.sid}`);
			cookies.delete(cookieName(name, secure), { path: '/', secure });
		},
		async endSid(sid) {
			const id = await r.get(`${prefix}_sid:${sid}`);
			if (id) await r.del(`${prefix}:${id}`);
			await r.del(`${prefix}_sid:${sid}`);
		},
		async ended() {
			return false;
		},
		async locked(session, fn) {
			const id = idOf.get(session) ?? session.sid;
			const lock = `${prefix}_lock:${id}`;
			// The lock holds its owner's token: a refresh that outlives the lock does not release
			// the next owner's (get and del are two commands; the window is the time between them).
			const mine = newId();
			for (let i = 0; i < 50; i++) {
				if (await r.set(lock, mine, { px: 10_000, nx: true })) {
					try {
						return await fn();
					} finally {
						if ((await r.get(lock)) === mine) await r.del(lock);
					}
				}
				await new Promise((res) => setTimeout(res, 100));
			}
			throw new Error('the session stayed locked');
		}
	};
}
