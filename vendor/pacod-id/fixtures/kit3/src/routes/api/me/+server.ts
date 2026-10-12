import { json } from '@sveltejs/kit';

// The session's tokens as the server holds them now (the end-to-end tests refresh through it).
export const GET = ({ locals }) =>
	locals.session
		? json({
				sub: locals.session.sub,
				sid: locals.session.sid,
				access: locals.session.accessToken,
				exp: locals.session.accessExpiresAt
			})
		: json({ error: 'signed out' }, { status: 401 });
