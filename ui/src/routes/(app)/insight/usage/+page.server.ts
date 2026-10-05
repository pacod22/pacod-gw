import { loadReport, SCOPE, tagKeysOf, USAGE_FILTERS } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	const filters = ['provider', 'model', 'template', 'key_id', 'end_user', 'tag.{key}'];
	const [overview, usage, accuracy, tagKeys] = await Promise.all([
		loadReport(event, '/analytics/overview', ['to', ...SCOPE, ...filters]),
		loadReport(event, '/analytics/usage', [...USAGE_FILTERS, 'bucket', 'group_by'], {
			bucket: 'hour',
			group_by: 'none'
		}),
		loadReport(event, '/analytics/accuracy', USAGE_FILTERS),
		tagKeysOf(event)
	]);
	return { overview, usage, accuracy, tagKeys, search: event.url.search };
};
