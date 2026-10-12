import { getLocale } from '#lib/paraglide/runtime.js';

/** A moment in the page's language, to the minute. */
export function formatTime(iso: string | null | undefined): string {
	if (!iso) return '—';
	return new Intl.DateTimeFormat(getLocale(), { dateStyle: 'medium', timeStyle: 'short' }).format(
		new Date(iso)
	);
}
