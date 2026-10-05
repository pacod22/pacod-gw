/**
 * The typed client of the management API. It exists on the server only: one per request,
 * carrying that request's session token.
 */
import createClient, { type Client } from 'openapi-fetch';
import { UNREACHABLE } from '#lib/api/errors.ts';
import type { paths } from '#lib/api/schema.d.ts';
import { spec } from './spec';

export type ApiClient = Client<paths>;

/** The base path of the API below the core's origin, from the OpenAPI document. */
export const API_BASE = spec.servers?.[0]?.url ?? '/api/v1';

/** How long the console waits for the core before it reports that it does not answer. */
const TIMEOUT_MS = 20_000;

/**
 * A `fetch` that answers instead of throwing when the core cannot be reached, so that every
 * call ends in the same `{ data, error, response }` and is shown like any other API error.
 */
async function fetchOrExplain(request: Request): Promise<Response> {
	try {
		return await fetch(request, {
			signal: AbortSignal.any([request.signal, AbortSignal.timeout(TIMEOUT_MS)])
		});
	} catch {
		return Response.json(
			{ error: { type: UNREACHABLE, message: 'the management API of the gateway did not answer' } },
			{ status: 502 }
		);
	}
}

export function createApi(origin: string, token: string | null): ApiClient {
	return createClient<paths>({
		baseUrl: `${origin}${API_BASE}`,
		headers: token ? { authorization: `Bearer ${token}` } : {},
		fetch: fetchOrExplain
	});
}

/** What every call of the client resolves to, reduced to what the console reads. */
export interface ApiResult<T = unknown> {
	data?: T;
	error?: unknown;
	response: Response;
}

interface LooseInit {
	params?: { path?: Record<string, string>; query?: Record<string, string> };
	body?: unknown;
}

/**
 * The same client for paths that are only known at run time. The resource kit takes its
 * paths from a descriptor and checks them against the OpenAPI document when it is created.
 */
export interface LooseClient {
	GET(path: string, init?: LooseInit): Promise<ApiResult>;
	POST(path: string, init?: LooseInit): Promise<ApiResult>;
	PUT(path: string, init?: LooseInit): Promise<ApiResult>;
	DELETE(path: string, init?: LooseInit): Promise<ApiResult>;
}

export function loose(api: ApiClient): LooseClient {
	return api as unknown as LooseClient;
}
