import { error, fail, redirect } from '@sveltejs/kit';
import { BENCH_URL } from '$app/env/private';
import type { BenchmarkReport } from '#lib/benchmarks.ts';
import { requirePlatformAdmin } from '#lib/server/tester.ts';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = async ({ locals, params }) => {
	requirePlatformAdmin(locals);
	const { data, response } = await locals.api.GET('/benchmark-reports/{id}', {
		params: { path: { id: params.id } }
	});
	if (!data)
		error(
			response.status === 404 ? 404 : 502,
			response.status === 404 ? 'There is no such report.' : 'The report could not be read.'
		);
	return {
		summary: data.summary,
		report: data.document as unknown as BenchmarkReport,
		benchUrl: BENCH_URL ?? null
	};
};

export const actions: Actions = {
	delete: async ({ locals, params }) => {
		requirePlatformAdmin(locals);
		const { response } = await locals.api.DELETE('/benchmark-reports/{id}', {
			params: { path: { id: params.id } }
		});
		if (!response.ok) return fail(response.status, { message: 'The report could not be deleted.' });
		redirect(303, '/platform/testing/benchmarks');
	}
};
