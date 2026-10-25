import { redirect } from '@sveltejs/kit';
import { SETTINGS_TOPICS, settingsPath } from '#lib/platform-settings.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = () => {
	redirect(307, settingsPath(SETTINGS_TOPICS[0].key));
};
