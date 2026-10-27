import { fail, redirect } from '@sveltejs/kit';
import { BENCH_URL, GATEWAY_API_URL } from '$app/env/private';
import { API_BASE } from '#lib/server/api.ts';
import { requirePlatformAdmin } from '#lib/server/tester.ts';
import type { Actions, PageServerLoad } from './$types';

/** A report is at most this long (pacod-gw-bench-report's MAX_BYTES). */
const MAX_BYTES = 1 << 20;

export const load: PageServerLoad = async ({ locals }) => {
	requirePlatformAdmin(locals);
	const { data, error } = await locals.api.GET('/benchmark-reports', {
		params: { query: { limit: 100 } }
	});
	const message = (error as { error?: { message?: string } } | undefined)?.error?.message;
	return {
		reports: data?.items ?? [],
		problem: error ? (message ?? 'The reports could not be read.') : null,
		benchUrl: BENCH_URL ?? null
	};
};

export const actions: Actions = {
	/** A report file, sent to the core as it is: uploaded again, it is the same report. */
	upload: async ({ locals, request }) => {
		const token = requirePlatformAdmin(locals);
		const file = (await request.formData()).get('report');
		if (!(file instanceof File) || file.size === 0)
			return fail(400, { message: 'Choose a report file.' });
		if (file.size > MAX_BYTES)
			return fail(413, { message: 'A benchmark report is at most 1 MiB.' });
		let response: Response;
		try {
			response = await fetch(`${GATEWAY_API_URL}${API_BASE}/benchmark-reports`, {
				method: 'POST',
				headers: {
					authorization: `Bearer ${token}`,
					'content-type': 'application/json',
					'x-gw-bench-source': 'upload'
				},
				body: await file.arrayBuffer()
			});
		} catch {
			return fail(502, { message: 'The management API of the gateway did not answer.' });
		}
		const body = await response.json().catch(() => null);
		if (!response.ok) {
			return fail(response.status, {
				message: body?.error?.message ?? `The gateway answered ${response.status}.`,
				problems: (body?.error?.details?.problems ?? []) as { path: string; message: string }[]
			});
		}
		redirect(303, `/platform/testing/benchmarks/${body.summary.id}`);
	}
};
