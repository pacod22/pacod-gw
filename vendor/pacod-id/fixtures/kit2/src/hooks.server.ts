import { createIdHandle, memoryStore, sealedCookie } from '@pacod/id-sveltekit';

// Read when the server starts (adapter-node): the same build runs on the host and in a container.
const env = process.env;
const origin = env.APP_ORIGIN ?? 'http://kit2.e2e.test:27391';
const secret = env.APP_SESSION_SECRET ?? 'a-fixture-session-secret-of-32-chars';

export const handle = createIdHandle({
	issuer: env.ID_ISSUER ?? 'http://id.gateway.e2e.test:27370',
	internalUrl: env.ID_INTERNAL_URL,
	clientId: env.ID_CLIENT_ID ?? 'demo-kit2',
	clientSecret: env.ID_CLIENT_SECRET,
	redirectUri: `${origin}/auth/callback`,
	postLogoutRedirectUri: `${origin}/`,
	origin,
	secret,
	store: env.APP_STORE === 'memory' ? memoryStore() : sealedCookie({ name: 'app_session', secret }),
	refreshBefore: Number(env.REFRESH_BEFORE ?? 60)
});
