import { error, fail } from '@sveltejs/kit';
import { done, type KitEvent } from '#lib/server/resource.ts';
import { requirePlatformAdmin, tester } from '#lib/server/tester.ts';
import { RUN_ID, type Run } from '#lib/testing.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	requirePlatformAdmin(event.locals);
	if (!RUN_ID.test(event.params.id)) error(404, 'There is no such run.');
	event.depends('app:testing-run');
	const run = await tester<Run>(event.locals, 'GET', `/runs/${event.params.id}`);
	if (run.problem?.status === 404) error(404, 'There is no such run.');
	return { run: run.data, problem: run.problem };
};

export const actions = {
	/** Stops the run after the step that is going; the suite's objects are still removed. */
	cancel: async (event: KitEvent) => {
		requirePlatformAdmin(event.locals);
		const id = String(event.params.id ?? '');
		if (!RUN_ID.test(id)) error(404, 'There is no such run.');
		const result = await tester(event.locals, 'POST', `/runs/${id}/cancel`);
		if (result.problem)
			return fail(result.problem.status || 502, {
				action: 'cancel',
				ok: false,
				problem: result.problem
			});
		return done('cancel', 'The run stops after the step that is going, then cleans up.');
	}
};
