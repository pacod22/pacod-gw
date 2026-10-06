import { fail, redirect } from '@sveltejs/kit';
import { localProblem, toProblem } from '#lib/api/errors.ts';
import { safeNext, setSession } from '#lib/server/session.ts';
import type { Actions, PageServerLoad } from './$types';

export const load: PageServerLoad = ({ locals, url }) => {
	if (locals.principal) redirect(303, safeNext(url.searchParams.get('next')));
	return {
		/** Why the user is here, when it is not a plain visit. */
		reason: url.searchParams.get('reason'),
		/** Where to go after signing in; kept through the form's action. */
		next: url.searchParams.get('next'),
		/** The address to sign in with, when a page that sent the user here knows it. */
		email: url.searchParams.get('email') ?? ''
	};
};

export const actions: Actions = {
	login: async ({ request, locals, cookies, url }) => {
		const form = await request.formData();
		const email = String(form.get('email') ?? '').trim();
		const password = String(form.get('password') ?? '');
		const remember = form.get('remember_me') != null;
		if (!email || !password) {
			return fail(400, {
				email,
				remember,
				problem: localProblem('Enter your email address and your password.')
			});
		}

		const { data, error, response } = await locals.api.POST('/session', {
			body: { email, password, remember_me: remember }
		});
		if (!response.ok || !data) {
			// The password is never sent back; the email is, so that it need not be typed again.
			const problem = toProblem(response.status, error);
			if (problem.type === 'email_not_verified')
				return fail(403, { email, remember, unverified: true });
			return fail(response.status, { email, remember, problem });
		}

		// The token goes into an httpOnly cookie and nowhere else. It lasts as long as the
		// session: "Keep me logged in" makes the session, and so the cookie, last days.
		setSession(cookies, data.token, data.expires_at);
		redirect(303, safeNext(url.searchParams.get('next')));
	},

	/** Sends a new verification link to an address that is not confirmed yet. */
	resend: async ({ request, locals }) => {
		const form = await request.formData();
		const email = String(form.get('email') ?? '').trim();
		if (!email) return fail(400, { email, problem: localProblem('Enter your email address.') });
		const { error, response } = await locals.api.POST('/session/verification', { body: { email } });
		if (!response.ok)
			return fail(response.status, { email, problem: toProblem(response.status, error) });
		return { email, resent: true };
	}
};
