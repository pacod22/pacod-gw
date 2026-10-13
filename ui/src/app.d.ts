import type { ApiProblem } from '#lib/api/errors.ts';
import type { Principal } from '#lib/api/types.ts';
import type { ApiClient } from '#lib/server/api.ts';
import type { IdSession } from '@pacod/id-sveltekit';

declare global {
	namespace App {
		interface Locals {
			/** The management API, called with this request's session. */
			api: ApiClient;
			/**
			 * The bearer the core takes: the session token from the cookie, or with pacod-id the
			 * session's access token. It never leaves the server.
			 */
			token: string | null;
			/** With pacod-id: the session (its tokens), refreshed when due. */
			session?: IdSession | null;
			/** Who is signed in, resolved once per request. */
			principal: Principal | null;
			/** Set when the core could not be asked who the caller is. */
			apiProblem: ApiProblem | null;
		}
	}
}

export {};
