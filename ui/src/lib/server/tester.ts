/**
 * The tester service, for the Testing pages: called from the server side of the console only,
 * with the session of the signed-in platform admin. The tester checks that session with the
 * core itself; the guard here keeps everyone else from reaching even the console's routes.
 */
import { error } from '@sveltejs/kit';
import { TESTER_URL } from '$app/env/private';
import type { ApiProblem } from '#lib/api/errors.ts';

/** Starting a run and reading the catalog are quick; a longer wait means the tester is stuck. */
const TIMEOUT_MS = 15_000;

export const testerConfigured = (): boolean => Boolean(TESTER_URL);

/**
 * Refuses everyone but a platform admin. Every load, form action and endpoint below
 * /platform/testing calls it: form actions and endpoints do not pass through the loads.
 */
export function requirePlatformAdmin(locals: App.Locals): string {
	if (!locals.principal || !locals.token) error(401, 'Sign in first.');
	if (locals.principal.platform_admin !== true) error(403, 'Testing is for platform admins only.');
	return locals.token;
}

export type TesterAnswer<T> = { data: T; problem: null } | { data: null; problem: ApiProblem };

function problem(
	status: number,
	title: string,
	message: string,
	more: Partial<ApiProblem> = {}
): ApiProblem {
	return {
		status,
		type: status ? `tester_${status}` : 'unreachable',
		title,
		message,
		hint: '',
		placement: 'notice',
		tone: 'error',
		reload: false,
		fields: {},
		lists: [],
		...more
	};
}

export const notConfigured = (): ApiProblem =>
	problem(
		0,
		'Testing is not set up',
		'The console has no TESTER_URL, so it cannot reach the tester.',
		{
			tone: 'info',
			hint: 'Deploy the tester container and give the console its address in TESTER_URL.'
		}
	);

const TITLES: Record<number, string> = {
	401: 'The session has ended',
	403: 'Testing is for platform admins only',
	404: 'Not found',
	409: 'A run is going',
	422: 'The tester refused this',
	502: 'The tester cannot reach the gateway'
};

/** Calls the tester. Answers are JSON, except for suite files (text). */
export async function tester<T>(
	locals: App.Locals,
	method: 'GET' | 'POST' | 'DELETE',
	path: string,
	body?: { json: unknown } | { text: string }
): Promise<TesterAnswer<T>> {
	const token = requirePlatformAdmin(locals);
	if (!TESTER_URL) return { data: null, problem: notConfigured() };
	const headers: Record<string, string> = { authorization: `Bearer ${token}` };
	let payload: string | undefined;
	if (body && 'json' in body) {
		headers['content-type'] = 'application/json';
		payload = JSON.stringify(body.json);
	} else if (body) {
		headers['content-type'] = 'application/yaml';
		payload = body.text;
	}
	let response: Response;
	try {
		response = await fetch(`${TESTER_URL}${path}`, {
			method,
			headers,
			body: payload,
			signal: AbortSignal.timeout(TIMEOUT_MS)
		});
	} catch (e) {
		const late = e instanceof Error && e.name === 'TimeoutError';
		return {
			data: null,
			problem: problem(
				0,
				'The tester does not answer',
				late ? 'The tester did not answer in time.' : 'The tester could not be reached.',
				{ hint: 'Check that the tester container is running.', tone: 'warning' }
			)
		};
	}
	if (response.status === 204) return { data: undefined as T, problem: null };
	const text = await response.text();
	let parsed: unknown = text;
	if ((response.headers.get('content-type') ?? '').includes('json')) {
		try {
			parsed = JSON.parse(text);
		} catch {
			parsed = text;
		}
	}
	if (response.ok) return { data: parsed as T, problem: null };

	const detail =
		parsed && typeof parsed === 'object' && 'detail' in parsed ? parsed.detail : undefined;
	const errors =
		parsed && typeof parsed === 'object' && 'errors' in parsed && Array.isArray(parsed.errors)
			? (parsed.errors as unknown[]).map(String)
			: [];
	return {
		data: null,
		problem: problem(
			response.status,
			TITLES[response.status] ?? 'The tester refused the request',
			(typeof detail === 'string'
				? detail
				: `The tester answered with status ${response.status}.`) +
				(errors.length ? ` ${errors.join('; ')}.` : ''),
			{
				tone: response.status === 409 ? 'warning' : 'error',
				placement: response.status === 422 ? 'form' : 'notice'
			}
		)
	};
}

/** Opens the tester's event stream of a run. Null when the tester cannot be reached. */
export async function testerStream(
	locals: App.Locals,
	path: string,
	signal: AbortSignal
): Promise<Response | null> {
	const token = requirePlatformAdmin(locals);
	if (!TESTER_URL) return null;
	try {
		return await fetch(`${TESTER_URL}${path}`, {
			headers: { authorization: `Bearer ${token}`, accept: 'text/event-stream' },
			signal
		});
	} catch {
		return null;
	}
}
