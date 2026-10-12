// The parts of SvelteKit this package uses, by shape, so that one build serves SvelteKit 2 and 3
// (and nothing imports `$app/*` or `$env/*`).

export interface CookieOptions {
	path: string;
	httpOnly?: boolean;
	secure?: boolean;
	sameSite?: 'lax' | 'strict' | 'none';
	maxAge?: number;
}

export interface Cookies {
	get(name: string): string | undefined;
	set(name: string, value: string, opts: CookieOptions): void;
	delete(name: string, opts: { path: string; secure?: boolean }): void;
}

export interface RequestEvent {
	url: URL;
	request: Request;
	cookies: Cookies;
	locals: object;
}

export type Resolve = (event: never) => Response | Promise<Response>;
