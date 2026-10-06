import { fail, redirect } from '@sveltejs/kit';
import { localProblem, toProblem } from '#lib/api/errors.ts';
import type { Actions, PageServerLoad } from './$types';

/** Console passwords must be at least this long, as the core requires. */
const MIN_PASSWORD_LEN = 12;

// Showing the page does not use up the link: only sending the form does.
export const load: PageServerLoad = ({ url }) => ({ token: url.searchParams.get('token') ?? '' });

export const actions: Actions = {
	default: async ({ request, locals }) => {
		const form = await request.formData();
		const token = String(form.get('token') ?? '');
		const password = String(form.get('password') ?? '');
		const repeat = String(form.get('repeat') ?? '');
		if (!token) return fail(400, { invalid: true });
		if (password.length < MIN_PASSWORD_LEN) {
			return fail(400, {
				problem: localProblem(`The password must be at least ${MIN_PASSWORD_LEN} characters long.`)
			});
		}
		if (password !== repeat) {
			return fail(400, { problem: localProblem('The two passwords are not the same.') });
		}
		const { error, response } = await locals.api.POST('/session/password-reset/confirm', {
			body: { token, password }
		});
		if (!response.ok) {
			const problem = toProblem(response.status, error);
			if (problem.type === 'invalid_token') return fail(400, { invalid: true });
			return fail(response.status, { problem });
		}
		redirect(303, '/login?reason=password-reset');
	}
};
