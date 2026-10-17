// @pacod/id-client: pacod-id for a Node application's server. Framework-free;
// @pacod/id-sveltekit builds on it.

export type { IdClientConfig } from './config.js';
export {
	IdClient,
	SignedOut,
	safeNext,
	type IdSession,
	type LoginOptions,
	type PendingLogin
} from './client.js';
export { SingleFlight } from './lock.js';
export {
	TokenError,
	verifyAccessToken,
	verifyLogoutToken,
	type AccessToken,
	type LogoutToken
} from './tokens.js';
