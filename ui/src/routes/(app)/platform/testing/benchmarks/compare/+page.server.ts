import { error } from '@sveltejs/kit';
import type { BenchmarkReport, BenchmarkSummary } from '#lib/benchmarks.ts';
import { requirePlatformAdmin } from '#lib/server/tester.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async ({ locals, url }) => {
	requirePlatformAdmin(locals);
	const ids = (url.searchParams.get('ids') ?? '').split(',').filter(Boolean);
	if (ids.length < 2 || ids.length > 6) error(400, 'Compare two to six reports.');
	const read = await Promise.all(
		ids.map((id) => locals.api.GET('/benchmark-reports/{id}', { params: { path: { id } } }))
	);
	const reports: { summary: BenchmarkSummary; report: BenchmarkReport }[] = [];
	for (const r of read) {
		if (!r.data) error(r.response.status === 404 ? 404 : 502, 'A report could not be read.');
		reports.push({
			summary: r.data.summary,
			report: r.data.document as unknown as BenchmarkReport
		});
	}
	return { reports };
};
