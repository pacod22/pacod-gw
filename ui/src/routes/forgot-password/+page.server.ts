import { fail, redirect } from '@sveltejs/kit';
import { localProblem, toProblem } from '#lib/api/errors.ts';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = ({ locals, url }) => {
	if (locals.principal) redirect(303, '/');
	return { email: url.searchParams.get('email') ?? '' };
};

export const actions: Actions = {
	default: async ({ request, locals }) => {
		const form = await request.formData();
		const email = String(form.get('email') ?? '').trim();
		if (!email) return fail(400, { email, problem: localProblem('Enter your email address.') });
		// The answer is the same whether or not an account exists for the address.
		const { error, response } = await locals.api.POST('/session/password-reset', {
			body: { email }
		});
		if (!response.ok)
			return fail(response.status, { email, problem: toProblem(response.status, error) });
		return { email, sent: true };
	}
};
