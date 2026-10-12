// Signing in, refreshing and signing out for an application's server (a BFF), with oauth4webapi.
// The server metadata is built here rather than discovered: the browser endpoints are on the
// public issuer, everything the server calls itself on the internal address (inside a network,
// the issuer's name may not reach pacod-id at all).

import * as oauth from 'oauth4webapi';
import { createRemoteJWKSet, jwtVerify, type JWTVerifyGetKey } from 'jose';
import type { IdClientConfig } from './config.js';
import { SingleFlight } from './lock.js';
import {
	verifyAccessToken,
	verifyLogoutToken,
	type AccessToken,
	type LogoutToken
} from './tokens.js';

/** A signed-in session as an application keeps it. No id_token: sign-out is server to server. */
export interface IdSession {
	sub: string;
	sid: string;
	accessToken: string;
	/** Unix seconds. */
	accessExpiresAt: number;
	refreshToken?: string;
	name?: string;
	email?: string;
	/** Unix seconds. */
	createdAt: number;
}

/** What the application keeps between sending the browser to sign in and its return. */
export interface PendingLogin {
	state: string;
	nonce: string;
	verifier: string;
	/** Where to go afterwards: a path of the application. */
	next: string;
}

/** The session has ended at pacod-id (signed out, revoked, expired): the application drops it. */
export class SignedOut extends Error {}

export interface LoginOptions {
	next?: string;
	/** `login` asks for the password again; `create` opens sign-up. */
	prompt?: 'login' | 'create' | 'none';
	uiLocales?: string;
	loginHint?: string;
}

/**
 * Only a path of the application: no scheme, no other host. Browsers drop tabs and newlines
 * from addresses (`/\t/evil.example` is `//evil.example`), so the value is read as a browser
 * reads it and must stay on the application's own origin.
 */
export function safeNext(next: string | null | undefined): string {
	// eslint-disable-next-line no-control-regex
	if (!next || !next.startsWith('/') || /[\u0000-\u0020\u007f\\]/.test(next)) return '/';
	const base = 'http://app.invalid';
	try {
		const u = new URL(next, base);
		return u.origin === base ? u.pathname + u.search + u.hash : '/';
	} catch {
		return '/';
	}
}

const http = (url: string) => url.startsWith('http:');

export class IdClient {
	readonly config: IdClientConfig;
	private as: oauth.AuthorizationServer;
	private client: oauth.Client;
	private auth: oauth.ClientAuth;
	private keys: JWTVerifyGetKey;
	private refreshes = new SingleFlight<IdSession>();
	private options: { [oauth.allowInsecureRequests]?: boolean };

	constructor(config: IdClientConfig) {
		this.config = config;
		const issuer = config.issuer.replace(/\/$/, '');
		const internal = (config.internalUrl ?? issuer).replace(/\/$/, '');
		this.as = {
			issuer,
			authorization_endpoint: `${issuer}/oidc/authorize`,
			end_session_endpoint: `${issuer}/oidc/logout`,
			token_endpoint: `${internal}/oidc/token`,
			jwks_uri: `${internal}/oidc/jwks`,
			revocation_endpoint: `${internal}/oidc/revoke`,
			authorization_response_iss_parameter_supported: true,
			code_challenge_methods_supported: ['S256']
		};
		this.client = {
			client_id: config.clientId,
			id_token_signed_response_alg: config.idTokenAlg ?? 'RS256'
		};
		this.auth = config.clientSecret ? oauth.ClientSecretBasic(config.clientSecret) : oauth.None();
		this.keys = createRemoteJWKSet(new URL(`${internal}/oidc/jwks`), { cooldownDuration: 30_000 });
		// Plain HTTP only where it is used (development, or the internal address inside a network).
		this.options = http(internal) || http(issuer) ? { [oauth.allowInsecureRequests]: true } : {};
	}

	private get internal() {
		return (this.config.internalUrl ?? this.config.issuer).replace(/\/$/, '');
	}

	/** Where to send the browser, and what to keep until it returns. */
	async startLogin(opts: LoginOptions = {}): Promise<{ url: string; pending: PendingLogin }> {
		const verifier = oauth.generateRandomCodeVerifier();
		const pending: PendingLogin = {
			state: oauth.generateRandomState(),
			nonce: oauth.generateRandomNonce(),
			verifier,
			next: safeNext(opts.next)
		};
		const url = new URL(this.as.authorization_endpoint!);
		const q = url.searchParams;
		q.set('response_type', 'code');
		q.set('client_id', this.config.clientId);
		q.set('redirect_uri', this.config.redirectUri);
		q.set('scope', this.config.scope ?? 'openid email profile offline_access');
		q.set('state', pending.state);
		q.set('nonce', pending.nonce);
		q.set('code_challenge', await oauth.calculatePKCECodeChallenge(verifier));
		q.set('code_challenge_method', 'S256');
		if (opts.prompt) q.set('prompt', opts.prompt);
		if (opts.uiLocales) q.set('ui_locales', opts.uiLocales);
		if (opts.loginHint) q.set('login_hint', opts.loginHint);
		return { url: url.href, pending };
	}

	/** The browser is back at the callback: the code for tokens, checked, as a session. */
	async finishLogin(callback: URL, pending: PendingLogin): Promise<IdSession> {
		// The RFC 9207 `iss` and the state.
		const params = oauth.validateAuthResponse(this.as, this.client, callback, pending.state);
		const response = await oauth.authorizationCodeGrantRequest(
			this.as,
			this.client,
			this.auth,
			params,
			this.config.redirectUri,
			pending.verifier,
			this.options
		);
		const tokens = await oauth.processAuthorizationCodeResponse(this.as, this.client, response, {
			expectedNonce: pending.nonce,
			requireIdToken: true
		});
		// The id_token came straight from the token endpoint; its signature is checked too, since the
		// internal address may be plain HTTP.
		const { payload } = await jwtVerify(tokens.id_token!, this.keys, {
			issuer: this.as.issuer,
			audience: this.config.clientId,
			algorithms: ['RS256', 'ES256']
		});
		if (typeof payload.sid !== 'string') throw new SignedOut('the id_token names no session');
		return this.session(tokens, {
			sub: String(payload.sub),
			sid: payload.sid,
			name: typeof payload.name === 'string' ? payload.name : undefined,
			email: typeof payload.email === 'string' ? payload.email : undefined,
			createdAt: Math.floor(Date.now() / 1000)
		});
	}

	private session(
		tokens: oauth.TokenEndpointResponse,
		base: Pick<IdSession, 'sub' | 'sid' | 'name' | 'email' | 'createdAt'>
	): IdSession {
		return {
			...base,
			accessToken: tokens.access_token,
			accessExpiresAt: Math.floor(Date.now() / 1000) + (tokens.expires_in ?? 300),
			refreshToken: tokens.refresh_token
		};
	}

	/**
	 * New tokens for a session, once per session at a time in this process: parallel requests get
	 * the same answer. A refused refresh means the session has ended: {@link SignedOut}.
	 */
	async refresh(session: IdSession): Promise<IdSession> {
		const refreshToken = session.refreshToken;
		if (!refreshToken) throw new SignedOut('no refresh token');
		return this.refreshes.run(refreshToken, async () => {
			// Bounded: a pacod-id that does not answer must not hold the session's requests.
			const response = await oauth.refreshTokenGrantRequest(
				this.as,
				this.client,
				this.auth,
				refreshToken,
				{ ...this.options, signal: AbortSignal.timeout(10_000) }
			);
			try {
				const tokens = await oauth.processRefreshTokenResponse(this.as, this.client, response);
				return this.session(tokens, session);
			} catch (e) {
				if (e instanceof oauth.ResponseBodyError) throw new SignedOut(e.error);
				throw e;
			}
		});
	}

	/**
	 * Ends the session at pacod-id, server to server, and returns where to send the browser:
	 * pacod-id's sign-out, which comes back to `postLogoutRedirectUri`.
	 */
	async signOut(session: IdSession | null): Promise<string> {
		if (session && this.config.clientSecret) {
			const basic = Buffer.from(
				`${encodeURIComponent(this.config.clientId)}:${encodeURIComponent(this.config.clientSecret)}`
			).toString('base64');
			// The internal listener only: on the public one this is a 404, and the SSO session
			// would live on (the next sign-in would be silent).
			const r = await fetch(
				`${this.internal}/internal/v1/sessions/${encodeURIComponent(session.sid)}/revoke`,
				{
					method: 'POST',
					headers: { authorization: `Basic ${basic}` },
					signal: AbortSignal.timeout(10_000)
				}
			).catch((e: Error) => e);
			if (r instanceof Error || !r.ok) {
				console.warn(
					`pacod-id did not end the session (${r instanceof Error ? r.message : `HTTP ${r.status}`}); is internalUrl pacod-id's internal listener?`
				);
			}
		}
		if (session?.refreshToken) {
			await oauth
				.revocationRequest(this.as, this.client, this.auth, session.refreshToken, this.options)
				.catch(() => undefined);
		}
		return this.logoutUrl();
	}

	/** pacod-id's sign-out page for this client. */
	logoutUrl(state?: string): string {
		const url = new URL(this.as.end_session_endpoint!);
		url.searchParams.set('client_id', this.config.clientId);
		if (this.config.postLogoutRedirectUri)
			url.searchParams.set('post_logout_redirect_uri', this.config.postLogoutRedirectUri);
		if (state) url.searchParams.set('state', state);
		return url.href;
	}

	/** A back-channel logout token for this client (the request body's `logout_token`). */
	verifyLogoutToken(token: string): Promise<LogoutToken> {
		return verifyLogoutToken(token, this.keys, {
			issuer: this.as.issuer,
			clientId: this.config.clientId
		});
	}

	/** An access token for this application's own API. */
	verifyAccessToken(token: string): Promise<AccessToken> {
		if (!this.config.audience)
			throw new Error('verifyAccessToken needs `audience` in the configuration');
		return verifyAccessToken(token, this.keys, {
			issuers: [this.as.issuer, ...(this.config.extraIssuers ?? [])],
			audience: this.config.audience
		});
	}
}
