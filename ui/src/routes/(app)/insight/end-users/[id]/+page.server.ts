import { rangeOf, rangeQuery } from '#lib/report/analytics.ts';
import { loadReport, SCOPE } from '#lib/server/report.ts';
import type { PageServerLoad } from './$types';

export const load: PageServerLoad = async (event) => {
	// One person's activity reads best over a month, as in PACOD.
	const range = rangeOf(event.url.searchParams.get('range'), '30d');
	const { from, bucket } = rangeQuery(range, Date.now());
	const [report, usage] = await Promise.all([
		loadReport(
			event,
			`/analytics/end-users/${encodeURIComponent(event.params.id)}`,
			SCOPE,
			{},
			{
				from
			}
		),
		loadReport(
			event,
			'/analytics/usage',
			SCOPE,
			{},
			{
				from,
				bucket,
				group_by: 'none',
				end_user: event.params.id
			}
		)
	]);
	return { report, usage, filters: { ...report.filters, range: range.id } };
};
