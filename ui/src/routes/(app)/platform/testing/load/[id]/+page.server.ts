import { error, fail } from '@sveltejs/kit';
import type { LoadRun } from '#lib/load.ts';
import { done, type KitEvent } from '#lib/server/resource.ts';
import { requirePlatformAdmin, tester } from '#lib/server/tester.ts';
import { RUN_ID } from '#lib/testing.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	requirePlatformAdmin(event.locals);
	if (!RUN_ID.test(event.params.id)) error(404, 'There is no such load test.');
	event.depends('app:testing-load');
	const run = await tester<LoadRun>(event.locals, 'GET', `/load/runs/${event.params.id}`);
	if (run.problem?.status === 404) error(404, 'There is no such load test.');
	return { run: run.data, problem: run.problem };
};

export const actions = {
	/** Stops the agents at once; what the run set up is still removed. */
	cancel: async (event: KitEvent) => {
		requirePlatformAdmin(event.locals);
		const id = String(event.params.id ?? '');
		if (!RUN_ID.test(id)) error(404, 'There is no such load test.');
		const result = await tester(event.locals, 'POST', `/load/runs/${id}/cancel`);
		if (result.problem)
			return fail(result.problem.status || 502, {
				action: 'cancel',
				ok: false,
				problem: result.problem
			});
		return done('cancel', 'The agents stop now; then the load test cleans up.');
	}
};
