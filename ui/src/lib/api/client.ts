// Thin client for the gateway admin API and the analytics reports API.
import { PUBLIC_ANALYTICS_URL, PUBLIC_GATEWAY_ADMIN_URL } from '$app/env/public';

const GATEWAY = PUBLIC_GATEWAY_ADMIN_URL;
const ANALYTICS = PUBLIC_ANALYTICS_URL;

async function get<T>(base: string, path: string, fetchFn: typeof fetch = fetch): Promise<T> {
	const res = await fetchFn(`${base}${path}`);
	if (!res.ok) throw new Error(`${res.status} ${res.statusText}: ${path}`);
	return res.json() as Promise<T>;
}

export const gatewayApi = {
	health: (f?: typeof fetch) => get<{ status: string }>(GATEWAY, '/healthz', f)
};

export const analyticsApi = {
	health: (f?: typeof fetch) => get<{ status: string }>(ANALYTICS, '/healthz', f)
};
