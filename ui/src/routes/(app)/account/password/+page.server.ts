import { fail, redirect } from '@sveltejs/kit';
import { localProblem, toProblem, type ApiProblem } from '#lib/api/errors.ts';
import { clearSession, endSession } from '#lib/server/session.ts';
import type { Actions } from './$types';

export const actions: Actions = {
	default: async ({ request, locals, cookies, url }) => {
		const form = await request.formData();
		const current = String(form.get('current') ?? '');
		const next = String(form.get('new') ?? '');
		const repeated = String(form.get('repeat') ?? '');
		if (!current || !next) {
			return fail(400, { problem: localProblem('Enter your current password and a new one.') });
		}
		if (next !== repeated) {
			return fail(400, {
				problem: localProblem('The new password and its repetition differ.', {
					repeat: 'Type the new password again, exactly as above.'
				})
			});
		}

		const { error, response } = await locals.api.PUT('/session/password', {
			body: { current, new: next }
		});
		if (!response.ok) {
			if (response.status === 401) endSession({ cookies, url });
			// The core names the field: `current` for a wrong current password.
			const answered = toProblem(response.status, error, ['current', 'new']);
			const fields = { ...answered.fields };
			if (fields.password) fields.new = fields.password;
			if (answered.placement === 'form' && Object.keys(fields).length === 0) {
				fields.new = answered.message;
			}
			const problem: ApiProblem = {
				...answered,
				title: 'The password was not changed',
				hint: '',
				fields
			};
			return fail(response.status, { problem });
		}

		// The core ended every session of the user, this one included.
		clearSession(cookies);
		redirect(303, '/login?reason=password-changed');
	}
};
