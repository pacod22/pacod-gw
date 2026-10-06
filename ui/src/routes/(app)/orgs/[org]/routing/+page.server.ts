import { done, failed, formText, resource, type KitEvent } from '#lib/server/resource.ts';

// The list and the deletion come from the kit; a scheme is created and edited on pages of
// its own, because its rules are not a generic form.
const schemes = resource({
	path: '/orgs/{org}/routing-schemes',
	noun: 'routing scheme',
	// The scheme the organisation itself assigns, and whether it is locked.
	extra: async (event) => {
		const assignment = await event.locals.api.GET('/orgs/{org}/routing-assignment', {
			params: { path: { org: event.params.org ?? '' } }
		});
		return { assignment: assignment.data ?? null };
	}
});

export const load = schemes.load;
export const actions = {
	delete: schemes.actions.delete,

	/** The organisation's scheme and its lock: the organisation's administrators. */
	assign: async (event: KitEvent) => {
		const form = await event.request.formData();
		const version = Number(formText(form, 'version'));
		const result = await event.locals.api.PUT('/orgs/{org}/routing-assignment', {
			params: { path: { org: event.params.org ?? '' } },
			body: {
				scheme: formText(form, 'scheme') || null,
				locked: formText(form, 'locked') === 'on',
				version: Number.isInteger(version) && version > 0 ? version : null
			}
		});
		if (!result.response.ok || !result.data) return failed(event, 'assign', result);
		const d = result.data;
		return done(
			'assign',
			d.scheme
				? `The organisation now assigns “${d.scheme}”${d.locked ? ', locked for every workspace and template' : ''}.`
				: `The organisation assigns no scheme${d.locked ? ', locked: templates use their models by priority' : ''}.`
		);
	}
};
