import { error } from '@sveltejs/kit';
import { SETTINGS_TOPICS } from '#lib/platform-settings.ts';
import { objectResource, type KitEvent } from '#lib/server/resource.ts';

// One page per topic, each showing and saving only its own part of the settings.
const pages = new Map(
	SETTINGS_TOPICS.map((topic) => [
		topic.key,
		{ topic, settings: objectResource({ path: '/settings', noun: 'settings', paths: topic.paths }) }
	])
);

function pageOf(event: KitEvent) {
	const found = pages.get(event.params.topic ?? '');
	if (!found) error(404, 'There is no such page of the platform settings.');
	return found;
}

export const load = async (event: KitEvent) => {
	const { topic, settings } = pageOf(event);
	return { ...(await settings.load(event)), topic };
};

export const actions = {
	save: (event: KitEvent) => pageOf(event).settings.actions.save(event)
};
