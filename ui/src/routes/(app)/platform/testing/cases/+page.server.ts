import { fail } from '@sveltejs/kit';
import { localProblem } from '#lib/api/errors.ts';
import { done, formText, type KitEvent } from '#lib/server/resource.ts';
import { requirePlatformAdmin, tester } from '#lib/server/tester.ts';
import type { Catalog } from '#lib/testing.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	requirePlatformAdmin(event.locals);
	const catalog = await tester<Catalog>(event.locals, 'GET', '/catalog');
	return { suites: catalog.data?.suites ?? [], problem: catalog.problem };
};

/** The suite file from the upload or, without one, from the text field. */
async function suiteText(form: FormData): Promise<string> {
	const file = form.get('file');
	const text =
		file instanceof File && file.size > 0 ? await file.text() : String(form.get('text') ?? '');
	if (!text.trim()) throw new Error('Choose a suite file, or paste its content.');
	return text;
}

export const actions = {
	/** Adds a suite, or replaces the uploaded suite of the same name. */
	upload: async (event: KitEvent) => {
		requirePlatformAdmin(event.locals);
		let text: string;
		try {
			text = await suiteText(await event.request.formData());
		} catch (e) {
			return fail(400, {
				action: 'upload',
				ok: false,
				problem: localProblem(e instanceof Error ? e.message : 'The file could not be read.')
			});
		}
		const result = await tester<{ suite: string; cases: number }>(event.locals, 'POST', '/cases', {
			text
		});
		if (result.problem)
			return fail(result.problem.status || 502, {
				action: 'upload',
				ok: false,
				problem: result.problem
			});
		return done(
			'upload',
			`Suite “${result.data.suite}” with ${result.data.cases} cases is in the catalog.`
		);
	},

	/** Removes an uploaded suite. Built-in suites stay. */
	delete: async (event: KitEvent) => {
		requirePlatformAdmin(event.locals);
		const suite = formText(await event.request.formData(), 'suite');
		const result = await tester(event.locals, 'DELETE', `/cases/${encodeURIComponent(suite)}`);
		if (result.problem)
			return fail(result.problem.status || 502, {
				action: 'delete',
				ok: false,
				problem: result.problem
			});
		return done('delete', `Suite “${suite}” was removed.`);
	}
};
