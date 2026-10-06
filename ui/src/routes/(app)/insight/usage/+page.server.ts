import { loadReport, reportQuery, SCOPE, tagKeysOf, USAGE_FILTERS } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const filters = [
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
	// The split as it is sent: "tag" without a key falls back to none.
	const groupBy = reportQuery(event.url, ['group_by'], { group_by: 'none' }).query.group_by;
	const [overview, usage, byModelOverTime, byProvider, byModel, accuracy, tagKeys] =
		await Promise.all([
			loadReport(event, '/analytics/overview', ['to', ...SCOPE, ...filters]),
			loadReport(event, '/analytics/usage', [...USAGE_FILTERS, 'bucket', 'group_by'], {
				bucket: 'hour',
				group_by: 'none'
			}),
			// The stacked chart splits by model unless the page is split by something else.
			groupBy === 'none'
				? loadReport(
						event,
						'/analytics/usage',
						[...USAGE_FILTERS, 'bucket'],
						{ bucket: 'hour' },
						{ group_by: 'model' }
					)
				: null,
			loadReport(
				event,
				'/analytics/breakdown',
				USAGE_FILTERS,
				{},
				{ by: 'provider', limit: '1000' }
			),
			loadReport(event, '/analytics/breakdown', USAGE_FILTERS, {}, { by: 'model', limit: '1000' }),
			loadReport(event, '/analytics/accuracy', USAGE_FILTERS),
			tagKeysOf(event)
		]);
	return {
		overview,
		usage,
		stack: byModelOverTime ?? usage,
		byProvider,
		byModel,
		accuracy,
		tagKeys,
		search: event.url.search
	};
};
