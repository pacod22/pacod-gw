// @pacod/id-sveltekit: pacod-id for a SvelteKit 2 or 3 application.

export { createIdHandle, type IdHandleConfig, type IdLocals } from './handle.js';
export {
	memoryStore,
	redisStore,
	sealedCookie,
	type RedisLike,
	type SessionStore
} from './stores.js';
export type { Cookies, CookieOptions, RequestEvent } from './types.js';
export type { IdSession } from '@pacod/id-client';
