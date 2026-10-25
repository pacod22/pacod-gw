import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';
import { SETTINGS_TABS, SETTINGS_TOPICS, settingsTopic } from './platform-settings';
import { buildFields } from './schema/form';
import { describeObject, type OpenApiDoc } from './schema/spec';

// The real document: a setting the API gains must find a topic, or no page would show it.
const doc = JSON.parse(
	readFileSync(new URL('./api/openapi.json', import.meta.url), 'utf8')
) as OpenApiDoc;

describe('platform settings topics', () => {
	const input = describeObject(doc, '/settings').replace?.input;
	const fields = input ? buildFields(doc, input, { mode: 'replace' }) : [];
	const listed = SETTINGS_TOPICS.flatMap((topic) => topic.paths);

	it('show every setting the API accepts', () => {
		const settings = fields.map((f) => f.path).filter((path) => path !== 'version');
		expect(settings.length).toBeGreaterThan(0);
		expect([...listed].sort()).toEqual([...settings].sort());
	});

	it('show each setting once', () => {
		expect(new Set(listed).size).toBe(listed.length);
	});

	it('have one tab each, found by key', () => {
		expect(SETTINGS_TABS.map((tab) => tab.href)).toEqual(
			SETTINGS_TOPICS.map((topic) => `/platform/settings/${topic.key}`)
		);
		expect(settingsTopic('sign-in')?.title).toBe('Sign-in');
		expect(settingsTopic('nothing')).toBeUndefined();
	});
});
