import { principalOptions } from '#lib/server/options.ts';
import { resource } from '#lib/server/resource.ts';

// A list only: the API offers no way to write to the audit log. The filters
// `organisation_id` and `workspace_id` pass through from the page's query.
const audit = resource({
	path: '/audit-log',
	noun: 'audit entry',
	pageSize: 50,
	extra: async (event) => {
		// Names for the actors, where the caller may list users.
		const principals = event.locals.principal?.platform_admin ? await principalOptions(event) : [];
		return { actors: Object.fromEntries(principals.map((p) => [p.value, p.label])) };
	}
});

export const load = audit.load;
