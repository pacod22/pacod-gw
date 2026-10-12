export const load = ({ locals }) => ({
	who: locals.session
		? { sub: locals.session.sub, sid: locals.session.sid, email: locals.session.email ?? null }
		: null
});
