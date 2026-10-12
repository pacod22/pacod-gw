// Checks of the tokens pacod-id sends to an application's server: back-channel logout tokens
// (OIDC Back-Channel Logout 1.0) and, for an application's own API, access tokens.

import { jwtVerify, type JWTVerifyGetKey } from 'jose';

const BACKCHANNEL_EVENT = 'http://schemas.openid.net/event/backchannel-logout';

export class TokenError extends Error {}

/** What a logout token says: the session that ended, and its account. */
export interface LogoutToken {
	sid: string;
	sub?: string;
}

/** Logout tokens seen, by `jti`, so a replayed one is refused (they live two minutes). */
const seen = new Map<string, number>();

export async function verifyLogoutToken(
	token: string,
	keys: JWTVerifyGetKey,
	expect: { issuer: string; clientId: string }
): Promise<LogoutToken> {
	let payload;
	try {
		({ payload } = await jwtVerify(token, keys, {
			issuer: expect.issuer,
			audience: expect.clientId,
			typ: 'logout+jwt',
			algorithms: ['RS256', 'ES256'],
			maxTokenAge: '5m',
			// pacod-id's clock may run a little ahead: without leeway a token is "from the future"
			// and every delivery fails.
			clockTolerance: 30,
			requiredClaims: ['iat', 'jti']
		}));
	} catch (e) {
		throw new TokenError(`not a logout token of pacod-id: ${(e as Error).message}`);
	}
	const events = payload.events as Record<string, unknown> | undefined;
	if (!events || typeof events !== 'object' || !(BACKCHANNEL_EVENT in events))
		throw new TokenError('no back-channel logout event');
	if ('nonce' in payload) throw new TokenError('a logout token carries no nonce');
	if (typeof payload.sid !== 'string' || !payload.sid)
		throw new TokenError('no session in the logout token');
	const now = Date.now();
	for (const [k, until] of seen) if (until <= now) seen.delete(k);
	const jti = payload.jti as string;
	if (seen.has(jti)) throw new TokenError('the logout token was used already');
	seen.set(jti, now + 10 * 60_000);
	return { sid: payload.sid, sub: typeof payload.sub === 'string' ? payload.sub : undefined };
}

/** What an access token says. */
export interface AccessToken {
	sub: string;
	kind: 'user' | 'service';
	sid?: string;
	clientId: string;
	scope: string;
	issuer: string;
	expiresAt: number;
}

/** Checks an access token for an application's own API: `at+jwt`, issuer, audience, time. */
export async function verifyAccessToken(
	token: string,
	keys: JWTVerifyGetKey,
	expect: { issuers: string[]; audience: string }
): Promise<AccessToken> {
	let payload;
	try {
		({ payload } = await jwtVerify(token, keys, {
			issuer: expect.issuers,
			audience: expect.audience,
			typ: 'at+jwt',
			algorithms: ['RS256', 'ES256'],
			clockTolerance: 30
		}));
	} catch (e) {
		throw new TokenError(`not an access token for this application: ${(e as Error).message}`);
	}
	const kind = payload.pid_kind;
	if ((kind !== 'user' && kind !== 'service') || !payload.sub) throw new TokenError('no subject');
	if (kind === 'user' && typeof payload.sid !== 'string')
		throw new TokenError("a person's token without a session");
	return {
		sub: payload.sub,
		kind,
		sid: typeof payload.sid === 'string' ? payload.sid : undefined,
		clientId: String(payload.client_id ?? ''),
		scope: String(payload.scope ?? ''),
		issuer: String(payload.iss),
		expiresAt: Number(payload.exp)
	};
}
