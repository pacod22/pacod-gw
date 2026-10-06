import { fail, redirect } from '@sveltejs/kit';
import { localProblem, toProblem } from '#lib/api/errors.ts';
import { organisationPath, workspacePath } from '#lib/nav.ts';
import type { Actions, PageServerLoad } from './$types';

/** Console passwords must be at least this long, as the core requires. */
const MIN_PASSWORD_LEN = 12;

// Showing the page does not use up the invitation: only accepting does.
export const load: PageServerLoad = async ({ url, locals }) => {
	const token = url.searchParams.get('token') ?? '';
	if (!token) return { token, invitation: null, me: null };
	const { data } = await locals.api.POST('/session/invitation/lookup', { body: { token } });
	return {
		token,
		invitation: data ?? null,
		/** The address of whoever is signed in, to tell them apart from the invited one. */
		me: locals.principal?.kind === 'user' ? locals.principal.display : null
	};
};

export const actions: Actions = {
	/** Someone new: chooses a name and a password, then signs in. */
	create: async ({ request, locals }) => {
		const form = await request.formData();
		const token = String(form.get('token') ?? '');
		const displayName = String(form.get('display_name') ?? '').trim();
		const password = String(form.get('password') ?? '');
		const repeat = String(form.get('repeat') ?? '');
		if (password.length < MIN_PASSWORD_LEN) {
			return fail(400, {
				displayName,
				problem: localProblem(`The password must be at least ${MIN_PASSWORD_LEN} characters long.`)
			});
		}
		if (password !== repeat) {
			return fail(400, {
				displayName,
				problem: localProblem('The two passwords are not the same.')
			});
		}
		const { data, error, response } = await locals.api.POST('/session/invitation/accept', {
			body: { token, password, display_name: displayName || null }
		});
		if (!response.ok || !data) {
			const problem = toProblem(response.status, error);
			if (problem.type === 'invalid_token') return fail(400, { invalid: true });
			return fail(response.status, { displayName, problem });
		}
		// Signing in follows, once the address is confirmed where the link was handed on.
		const reason = data.needs_confirmation ? 'invitation-confirm' : 'invitation-accepted';
		redirect(303, `/login?${new URLSearchParams({ email: data.email, reason })}`);
	},

	/** Someone signed in to the invited account. */
	accept: async ({ request, locals }) => {
		const form = await request.formData();
		const token = String(form.get('token') ?? '');
		const { data, error, response } = await locals.api.POST('/session/invitation/accept', {
			body: { token, display_name: null, password: null }
		});
		if (!response.ok || !data) {
			const problem = toProblem(response.status, error);
			if (problem.type === 'invalid_token') return fail(400, { invalid: true });
			return fail(response.status, { problem });
		}
		// Off to what the invitation was for.
		redirect(
			303,
			data.workspace_id
				? workspacePath(data.organisation_id, data.workspace_id)
				: organisationPath(data.organisation_id)
		);
	}
};
