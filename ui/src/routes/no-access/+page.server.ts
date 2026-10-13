import { redirect } from '@sveltejs/kit';
import { invitationsUrl } from '#lib/identity.ts';
import { identity } from '#lib/server/identity.ts';
import type { PageServerLoad } from './$types';

/** Someone signed in through pacod-id who has no role in this gateway yet. */
export const load: PageServerLoad = ({ locals }) => {
	const principal = locals.principal;
	// Anyone with a role, and everyone with the console's own login, has the console.
	if (!identity || !principal || principal.roles.length > 0 || principal.platform_admin)
		redirect(303, '/');
	return { display: principal.display, invitations: invitationsUrl(identity.issuer) };
};
