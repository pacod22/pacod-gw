import { resource } from '#lib/server/resource.ts';

// The list and the deletion come from the kit; a scheme is created and edited on pages of
// its own, because its rules are not a generic form.
const schemes = resource({
	path: '/orgs/{org}/routing-schemes',
	noun: 'routing scheme'
});

export const load = schemes.load;
export const actions = { delete: schemes.actions.delete };
