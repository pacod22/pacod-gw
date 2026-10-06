import { requirePlatformAdmin, tester } from '#lib/server/tester.ts';
import type { RunSummary } from '#lib/testing.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	requirePlatformAdmin(event.locals);
	event.depends('app:testing-runs');
	const runs = await tester<{ items: RunSummary[]; active: string | null }>(
		event.locals,
		'GET',
		'/runs'
	);
	return { runs: runs.data?.items ?? [], active: runs.data?.active ?? null, problem: runs.problem };
};
