import { fail, redirect } from '@sveltejs/kit';
import { localProblem, toProblem } from '#lib/api/errors.ts';
import { safeNext, setSession } from '#lib/server/session.ts';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = ({ locals, url }) => {
	if (locals.principal) redirect(303, safeNext(url.searchParams.get('next')));
	return {
		/** Why the user is here, when it is not a plain visit. */
		reason: url.searchParams.get('reason')
	};
};

export const actions: Actions = {
	default: async ({ request, locals, cookies, url }) => {
		const form = await request.formData();
		const email = String(form.get('email') ?? '').trim();
		const password = String(form.get('password') ?? '');
		if (!email || !password) {
			return fail(400, {
				email,
				problem: localProblem('Enter your email address and your password.')
			});
		}

		const { data, error, response } = await locals.api.POST('/session', {
			body: { email, password }
		});
		if (!response.ok || !data) {
			// The password is never sent back; the email is, so that it need not be typed again.
			return fail(response.status, { email, problem: toProblem(response.status, error) });
		}

		// The token goes into an httpOnly cookie and nowhere else.
		setSession(cookies, data.token, data.expires_at);
		redirect(303, safeNext(url.searchParams.get('next')));
	}
};
