import type { ApiProblem } from '#lib/api/errors.ts';
import { parseRequestPage, type UsageEvent } from '#lib/insight/usage.ts';
import { toRfc3339 } from '#lib/schema/form.ts';
import { queryParams } from '#lib/schema/spec.ts';
import { loose } from '#lib/server/api.ts';
import { problemOf, queryFromUrl } from '#lib/server/resource.ts';
import { tagFilters, tagKeysOf } from '#lib/server/report.ts';
import { spec } from '#lib/server/spec.ts';
import type { PageServerLoad } from './$types';

// The analytics service refuses parameters it does not know, so only what the OpenAPI
// document declares for the request log is passed on.
const DECLARED = queryParams(spec, '/requests');

export const load: PageServerLoad = async (event) => {
	const query = queryFromUrl(DECLARED, event.url, { limit: '50' });
	// The filter form gives times without a zone; they are read as UTC.
	if (query.from) query.from = toRfc3339(query.from);
	if (query.to) query.to = toRfc3339(query.to);

	// Tag filters go along as `tag.<key>=value`.
	Object.assign(query, tagFilters(event.url));

	const [result, tagKeys] = await Promise.all([
		loose(event.locals.api).GET('/requests', { params: { query } }),
		tagKeysOf(event)
	]);

	let requests: UsageEvent[] = [];
	let nextCursor: string | null = null;
	let range: { from: string | null; to: string | null } = { from: null, to: null };
	let problem: ApiProblem | null = null;
	if (result.response.ok) {
		const page = parseRequestPage(result.data);
		requests = page.requests;
		nextCursor = page.nextCursor;
		range = { from: page.from, to: page.to };
	} else {
		problem = problemOf(event, result, DECLARED);
	}

	return {
		requests,
		nextCursor,
		cursor: event.url.searchParams.get('cursor'),
		range,
		problem,
		tagKeys,
		/** What the tag fields of the form show. */
		tags: Object.fromEntries(
			[...event.url.searchParams].filter(
				([name, value]) =>
					(name.startsWith('tag.') || name === 'tag_key' || name === 'tag_value') && value !== ''
			)
		),
		search: event.url.search,
		/** The filters as the form shows them. */
		filters: Object.fromEntries(
			DECLARED.filter((name) => name !== 'cursor' && name !== 'limit').map((name) => [
				name,
				event.url.searchParams.get(name) ?? ''
			])
		)
	};
};
