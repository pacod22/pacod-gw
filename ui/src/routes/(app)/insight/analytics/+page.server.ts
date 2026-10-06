import { rangeOf, rangeQuery } from '#lib/report/analytics.ts';
import { loadReport, SCOPE, tagKeysOf } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

/** The filters of the page; the range is one of the presets, sent as `from` and a bucket. */
const FILTERS = [
	...SCOPE,
	'provider',
	'model',
	'template',
	'key_id',
	'end_user',
	'agent_id',
	'user_id',
	'task',
	'complexity',
	'tag.{key}'
];

/** How many values a filter offers to choose from. */
const CHOICES = '1000';

export const load: PageServerLoad = async (event) => {
	const range = rangeOf(event.url.searchParams.get('range'));
	const { from, bucket } = rangeQuery(range, Date.now());
	// The values to choose from: those of the range and scope, whatever else is filtered, so a
	// filter still offers the other values once one is chosen.
	const choices = (fixed: Record<string, string>) =>
		loadReport(
			event,
			fixed.by ? '/analytics/breakdown' : '/analytics/end-users',
			SCOPE,
			{},
			{
				from,
				limit: CHOICES,
				...fixed
			}
		);
	const tagChoices = tagKeysOf(event).then(async (keys) => {
		if (!keys) return { keys, values: {} };
		const reports = await Promise.all(keys.map((key) => choices({ by: `tag:${key}` })));
		return { keys, values: Object.fromEntries(keys.map((key, i) => [key, reports[i]])) };
	});
	const [usage, errors, byModel, endUsers, models, templates, endUserChoices, agents, tasks, tags] =
		await Promise.all([
			loadReport(event, '/analytics/usage', FILTERS, {}, { from, bucket, group_by: 'none' }),
			loadReport(event, '/errors', FILTERS, {}, { from, bucket, limit: '1' }),
			loadReport(event, '/analytics/breakdown', FILTERS, {}, { from, by: 'model', limit: '50' }),
			// Enough to count the end users of most ranges; the page says when there are more.
			loadReport(event, '/analytics/end-users', FILTERS, {}, { from, limit: '1000' }),
			choices({ by: 'model' }),
			choices({ by: 'template' }),
			choices({}),
			choices({ by: 'agent_id' }),
			choices({ by: 'task' }),
			tagChoices
		]);
	return {
		range: range.id,
		filters: { ...usage.filters, range: range.id },
		usage,
		errors,
		byModel,
		endUsers,
		choices: { models, templates, endUsers: endUserChoices, agents, tasks, tags: tags.values },
		tagKeys: tags.keys
	};
};
