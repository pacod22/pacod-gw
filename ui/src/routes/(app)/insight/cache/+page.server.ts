import { rangeOf, rangeQuery } from '#lib/report/analytics.ts';
import { loadReport, SCOPE } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

/** The filters of the page; the range is one of the presets, sent as `from` and a bucket. */
const FILTERS = [...SCOPE, 'provider', 'model', 'template', 'key_id'];

export const load: PageServerLoad = async (event) => {
	const range = rangeOf(event.url.searchParams.get('range'));
	const { from, bucket } = rangeQuery(range, Date.now());
	const [cache, models] = await Promise.all([
		loadReport(event, '/analytics/cache', FILTERS, {}, { from, bucket }),
		// The models to choose from: those of the range and scope, whatever model is chosen.
		loadReport(event, '/analytics/breakdown', SCOPE, {}, { from, by: 'model', limit: '1000' })
	]);
	return { range: range.id, filters: { ...cache.filters, range: range.id }, cache, models };
};
