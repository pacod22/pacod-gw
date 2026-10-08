import { canWrite } from '#lib/access.ts';
import { loadedSchemesMessage } from '#lib/routing.ts';
import { done, failed, formText, resource, type KitEvent } from '#lib/server/resource.ts';
import { createMissingSchemes, defaultSchemes } from '#lib/server/routing.ts';

// The list and the deletion come from the kit; a scheme is created and edited on pages of
// its own, because its rules are not a generic form.
const schemes = resource({
	path: '/orgs/{org}/routing-schemes',
	noun: 'routing scheme',
	// The scheme the organisation itself assigns, whether it is locked, and for its
	// administrators what loading the default schemes would create.
	extra: async (event) => {
		const org = event.params.org ?? '';
		const principal = event.locals.principal;
		const writable = principal !== null && canWrite(principal, 'organisation', { org });
		const [assignment, defaults] = await Promise.all([
			event.locals.api.GET('/orgs/{org}/routing-assignment', { params: { path: { org } } }),
			writable ? defaultSchemes(event, org) : null
		]);
		return { assignment: assignment.data ?? null, defaults: defaults?.steps ?? null };
	}
});

export const load = schemes.load;
export const actions = {
	delete: schemes.actions.delete,

	/** Creates the default schemes the organisation lacks; those whose name is taken are kept. */
	loadDefaults: async (event: KitEvent) => {
		const org = event.params.org ?? '';
		const { defaults, list, steps } = await defaultSchemes(event, org);
		if (!defaults.response.ok) return failed(event, 'loadDefaults', defaults);
		if (!list.response.ok) return failed(event, 'loadDefaults', list);
		const made = await createMissingSchemes(event, 'loadDefaults', org, defaults.data!, steps!);
		if ('failure' in made) return made.failure;
		return done('loadDefaults', loadedSchemesMessage(made.created), {
			tone: made.created.length > 0 ? 'success' : 'warning'
		});
	},

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
