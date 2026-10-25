import { error } from '@sveltejs/kit';
import { hasPlatformRole } from '#lib/access.ts';
import { clearCache } from '#lib/server/plugins.ts';
import { problemOf, type KitEvent } from '#lib/server/resource.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const principal = event.locals.principal;
	if (!principal || !hasPlatformRole(principal)) {
		error(403, 'The response cache of the installation is for platform roles.');
	}
	const status = await event.locals.api.GET('/response-cache');
	return {
		status: status.data ?? null,
		problem: status.response.ok ? null : problemOf(event, status),
		canClear: principal.platform_admin
	};
};

export const actions = {
	clearCache: (event: KitEvent) => clearCache(event, 'platform')
};
