import type { ApiProblem } from '#lib/api/errors.ts';
import type { Principal } from '#lib/api/types.ts';
import type { ApiClient } from '#lib/server/api.ts';

declare global {
	namespace App {
		interface Locals {
			/** The management API, called with this request's session. */
			api: ApiClient;
			/** The session token from the cookie. It never leaves the server. */
			token: string | null;
			/** Who is signed in, resolved once per request. */
			principal: Principal | null;
			/** Set when the core could not be asked who the caller is. */
			apiProblem: ApiProblem | null;
		}
	}
}

export {};
