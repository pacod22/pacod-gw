import { fail, redirect } from '@sveltejs/kit';
import { localProblem } from '#lib/api/errors.ts';
import { formText, type KitEvent } from '#lib/server/resource.ts';
import { requirePlatformAdmin, tester } from '#lib/server/tester.ts';
import { liveModels } from '#lib/server/testing-models.ts';
import type { Catalog, RunSummary } from '#lib/testing.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	requirePlatformAdmin(event.locals);
	const [catalog, runs, models] = await Promise.all([
		tester<Catalog>(event.locals, 'GET', '/catalog'),
		tester<{ items: RunSummary[]; active: string | null }>(event.locals, 'GET', '/runs'),
		liveModels(event)
	]);
	return {
		catalog: catalog.data,
		problem: catalog.problem,
		active: runs.data?.active ?? null,
		last: runs.data?.items[0] ?? null,
		models
	};
};

export const actions = {
	/** Starts a run and opens its page. */
	start: async (event: KitEvent) => {
		requirePlatformAdmin(event.locals);
		const form = await event.request.formData();
		const mode = formText(form, 'mode');
		const cases = form.getAll('case').map(String).filter(Boolean);
		if (mode !== 'mock' && mode !== 'live')
			return fail(400, { action: 'start', ok: false, problem: localProblem('Choose a mode.') });
		if (cases.length === 0)
			return fail(400, {
				action: 'start',
				ok: false,
				problem: localProblem('Choose at least one case to run.')
			});
		const openai = formText(form, 'openai_model');
		const anthropic = formText(form, 'anthropic_model');
		const gemini = formText(form, 'gemini_model');
		const result = await tester<RunSummary>(event.locals, 'POST', '/runs', {
			json: {
				mode,
				cases,
				params:
					mode === 'live'
						? {
								openai_model: openai || null,
								anthropic_model: anthropic || null,
								gemini_model: gemini || null
							}
						: {},
				stop_on_fail: form.get('stop_on_fail') === 'on',
				keep_resources: form.get('keep_resources') === 'on'
			}
		});
		if (result.problem) {
			const status = result.problem.status >= 400 ? result.problem.status : 502;
			return fail(status, { action: 'start', ok: false, problem: result.problem });
		}
		redirect(303, `/platform/testing/runs/${result.data.id}`);
	}
};
