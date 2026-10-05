import { limitAmount } from '#lib/limits.ts';
import { resource, type KitEvent } from '#lib/server/resource.ts';

/** The end-user limits of the workspace: only those can be overridden per end user. */
async function endUserLimits(event: KitEvent) {
	const result = await event.locals.api.GET('/orgs/{org}/workspaces/{ws}/limits', {
		params: {
			path: { org: event.params.org ?? '', ws: event.params.ws ?? '' },
			query: { limit: 500 }
		}
	});
	return (result.data?.items ?? []).filter((limit) => limit.subject === 'end_user');
}

const overrides = resource({
	path: '/orgs/{org}/workspaces/{ws}/end-user-overrides',
	noun: 'override',
	nameOf: (override) => override.end_user,
	fields: async (event) => ({
		limit_id: {
			label: 'Limit',
			options: (await endUserLimits(event)).map((limit) => ({
				value: limit.id,
				label: `${limit.name} – ${limitAmount(limit)}`,
				help: ''
			}))
		}
	}),
	extra: async (event) => {
		const limits = await endUserLimits(event);
		return {
			limits: Object.fromEntries(limits.map((l) => [l.id, { name: l.name, metric: l.metric }]))
		};
	}
});

export const load = overrides.load;
export const actions = overrides.actions;
