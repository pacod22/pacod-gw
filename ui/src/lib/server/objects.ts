/** A workspace's files and message batches at the provider, created through the gateway. */
import type { ApiProblem } from '#lib/api/errors.ts';
import type { Schemas } from '#lib/api/types.ts';
import { keyOptions } from './options.ts';
import { problemOf, type KitEvent } from './resource.ts';

export type ProviderObject = Schemas['ProviderObject'];

/** One page of files or batches, with the names of the keys that created them. */
export async function loadObjects(event: KitEvent, kind: 'files' | 'batches') {
	const org = event.params.org ?? '';
	const ws = event.params.ws ?? '';
	const cursor = event.url.searchParams.get('cursor');
	const params = { path: { org, ws }, query: cursor ? { cursor } : {} };
	const [result, keys] = await Promise.all([
		kind === 'files'
			? event.locals.api.GET('/orgs/{org}/workspaces/{ws}/files', { params })
			: event.locals.api.GET('/orgs/{org}/workspaces/{ws}/batches', { params }),
		keyOptions(event, org, ws)
	]);
	const problem: ApiProblem | null = result.response.ok ? null : problemOf(event, result);
	return {
		items: result.data?.items ?? [],
		cursor,
		nextCursor: result.data?.next_cursor ?? null,
		keys: Object.fromEntries(keys.map((k) => [k.value, k.label])),
		problem
	};
}
