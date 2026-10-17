/** Where and as whom an application talks to pacod-id. */
export interface IdClientConfig {
	/**
	 * The realm's public issuer (`https://id.example.com`): the browser goes there to sign in and
	 * out, and tokens carry it as `iss`.
	 */
	issuer: string;
	/**
	 * pacod-id's internal listener as the application's server reaches it (`http://id:8071`):
	 * token, keys, revocation, and ending a session at sign-out, which only that listener
	 * serves. Defaults to the issuer, which suits a public client only.
	 */
	internalUrl?: string;
	clientId: string;
	/** A confidential client's secret; none for a public client. */
	clientSecret?: string;
	/** The application's callback, registered with the client (`https://app/auth/callback`). */
	redirectUri: string;
	/** Where pacod-id sends the browser after signing out, registered with the client. */
	postLogoutRedirectUri?: string;
	/** Default `openid email profile offline_access`. */
	scope?: string;
	/** The audience of the access tokens this application's own API takes (for `verifyAccessToken`). */
	audience?: string;
	/** Further issuers whose access tokens `verifyAccessToken` takes (shop's two realms). */
	extraIssuers?: string[];
	/** The client's `id_token_alg` in pacod-id's configuration; RS256 unless set there. */
	idTokenAlg?: 'RS256' | 'ES256';
}
