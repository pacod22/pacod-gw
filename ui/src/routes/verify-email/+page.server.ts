import { fail, redirect } from '@sveltejs/kit';
import { toProblem } from '#lib/api/errors.ts';
import type { Actions, PageServerLoad } from './$types';

// Showing the page does not use up the link. Mail scanners open links to check them; the
// address is confirmed only when the person presses the button.
export const load: PageServerLoad = ({ url }) => ({ token: url.searchParams.get('token') ?? '' });

export const actions: Actions = {
	default: async ({ request, locals }) => {
		const form = await request.formData();
		const token = String(form.get('token') ?? '');
		if (!token) return fail(400, { invalid: true });
		const { error, response } = await locals.api.POST('/session/verification/confirm', {
			body: { token }
		});
		if (!response.ok) {
			const problem = toProblem(response.status, error);
			if (problem.type === 'invalid_token') return fail(400, { invalid: true });
			return fail(response.status, { problem });
		}
		redirect(303, '/login?reason=email-verified');
	}
};
