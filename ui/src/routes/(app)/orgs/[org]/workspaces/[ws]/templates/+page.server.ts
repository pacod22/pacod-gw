import { resource, type KitEvent } from '#lib/server/resource.ts';
import { importTemplate } from '#lib/server/templates.ts';

/** How far back "last used" looks. */
const LAST_USED_DAYS = 90;

/** When each template of the workspace was last called. Empty when analytics cannot answer. */
async function lastUsed(event: KitEvent): Promise<Record<string, string>> {
	const from = new Date(Date.now() - LAST_USED_DAYS * 86_400_000).toISOString();
	const result = await event.locals.api.GET('/analytics/last-used', {
		params: {
			query: {
				by: 'template',
				from,
				organisation_id: event.params.org ?? '',
				workspace_id: event.params.ws ?? ''
			} as never
		}
	});
	const rows = (
		(result.data as { rows?: { group: string; last_used: string }[] } | undefined)?.rows ?? []
	).filter((r) => r.group);
	return Object.fromEntries(rows.map((r) => [r.group, r.last_used]));
}

/**
 * The organisation's templates as this workspace inherits them. `null` when the caller may
 * not see them: the page then leaves the section out.
 */
async function inherited(event: KitEvent) {
	const result = await event.locals.api.GET('/orgs/{org}/workspaces/{ws}/inherited-templates', {
		params: { path: { org: event.params.org ?? '', ws: event.params.ws ?? '' } }
	});
	return result.response.ok ? (result.data ?? []) : null;
}

// The list and the deletion come from the kit; a template is created and edited on pages
// of its own, because its form is not the generic one.
const templates = resource({
	path: '/orgs/{org}/workspaces/{ws}/templates',
	noun: 'template',
	extra: async (event) => {
		const [used, fromOrganisation] = await Promise.all([lastUsed(event), inherited(event)]);
		return { lastUsed: used, lastUsedDays: LAST_USED_DAYS, inherited: fromOrganisation };
	}
});

export const load = templates.load;
export const actions = {
	delete: templates.actions.delete,
	import: importTemplate
};
