import { fail, redirect } from '@sveltejs/kit';
import { localProblem } from '#lib/api/errors.ts';
import { loadRequestFrom, type AgentsAnswer, type LoadRunSummary } from '#lib/load.ts';
import type { KitEvent } from '#lib/server/resource.ts';
import { requirePlatformAdmin, tester } from '#lib/server/tester.ts';
import { activeModels } from '#lib/server/testing-models.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	requirePlatformAdmin(event.locals);
	// The agents are asked whether they are there; that takes up to a few seconds.
	const [agents, runs, models] = await Promise.all([
		tester<AgentsAnswer>(event.locals, 'GET', '/load/agents'),
		tester<{ items: LoadRunSummary[]; active: string | null }>(event.locals, 'GET', '/load/runs'),
		activeModels(event)
	]);
	return {
		agents: agents.data,
		problem: agents.problem,
		runs: runs.data?.items ?? [],
		active: runs.data?.active ?? null,
		models
	};
};

export const actions = {
	/** Starts a load test and opens its page. A pasted key is never sent back to the browser. */
	start: async (event: KitEvent) => {
		requirePlatformAdmin(event.locals);
		const form = await event.request.formData();
		const built = loadRequestFrom(form);
		if (built.error !== null)
			return fail(400, { action: 'start', ok: false, problem: localProblem(built.error) });
		// Preparing the agents happens after the answer: starting is quick.
		const result = await tester<LoadRunSummary>(event.locals, 'POST', '/load/runs', {
			json: built.request
		});
		if (result.problem) {
			const status = result.problem.status >= 400 ? result.problem.status : 502;
			return fail(status, { action: 'start', ok: false, problem: result.problem });
		}
		redirect(303, `/platform/testing/load/${result.data.id}`);
	}
};
