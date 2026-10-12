// Calls to pacod-id's /ui-api/v1, on the page's own origin (the session and flow cookies go
// with them; writes carry the Origin header the service checks).

export class ApiError extends Error {
	constructor(
		public status: number,
		/** The error's `details.code`, or its type. */
		public code: string,
		message: string,
		public details: Record<string, unknown> = {}
	) {
		super(message);
	}
}

// eslint-disable-next-line @typescript-eslint/no-explicit-any
export async function api<T = any>(method: string, path: string, body?: unknown): Promise<T> {
	const write = method !== 'GET' && method !== 'HEAD';
	const response = await fetch(`/ui-api/v1/${path}`, {
		method,
		credentials: 'same-origin',
		headers: write ? { 'content-type': 'application/json' } : {},
		body: write ? JSON.stringify(body ?? {}) : undefined
	});
	const text = await response.text();
	let data: unknown;
	try {
		data = text ? JSON.parse(text) : null;
	} catch {
		data = null;
	}
	if (!response.ok) {
		const error = (
			data as { error?: { type?: string; message?: string; details?: Record<string, unknown> } }
		)?.error;
		const code =
			(error?.details?.code as string | undefined) ?? error?.type ?? `http_${response.status}`;
		throw new ApiError(
			response.status,
			code,
			error?.message ?? response.statusText,
			error?.details ?? {}
		);
	}
	return data as T;
}

export const get = <T>(path: string) => api<T>('GET', path);
export const post = <T>(path: string, body?: unknown) => api<T>('POST', path, body);
export const patch = <T>(path: string, body?: unknown) => api<T>('PATCH', path, body);
export const del = <T>(path: string) => api<T>('DELETE', path);
