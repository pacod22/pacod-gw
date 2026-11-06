/** The price tracker's pages, as the Pricing pages page shows them. */

import type { Schemas } from '#lib/api/types.ts';

type Page = Schemas['Item16'];

/** The pages of each provider, providers by name; within one, the core's order (priority). */
export function pageGroups(
	pages: Page[]
): { provider_id: string; provider: string; pages: Page[] }[] {
	const groups = new Map<string, { provider_id: string; provider: string; pages: Page[] }>();
	for (const page of pages) {
		const group = groups.get(page.provider_id) ?? {
			provider_id: page.provider_id,
			// The core sends no name for a provider that was deleted since.
			provider: page.provider || page.provider_id,
			pages: []
		};
		group.pages.push(page);
		groups.set(page.provider_id, group);
	}
	return [...groups.values()].sort((a, b) =>
		a.provider.localeCompare(b.provider, 'en', { numeric: true })
	);
}

/** What the last check found, and what the scan said of the page, in a line. */
export function pageNote(page: Page): string {
	const s = page.state;
	const notes: string[] = [];
	if (page.status === 'proposed' && page.score != null) notes.push(`scored ${page.score}`);
	if (page.moved_from) notes.push(`replaces ${page.moved_from}`);
	if (s?.flag === 'gone') notes.push('gone: it answers 404 or 410');
	if (s?.flag === 'moved') notes.push(`moved to ${s.moved_to ?? 'another address'}`);
	if (s?.outcome) {
		const outcome = s.outcome.replaceAll('_', ' ');
		notes.push(s.message ? `${outcome}: ${s.message}` : outcome);
	}
	if (s && s.consecutive_failures > 1) notes.push(`${s.consecutive_failures} failures in a row`);
	if (s?.check_requested) notes.push('to be checked at the next run');
	return notes.join('; ');
}
