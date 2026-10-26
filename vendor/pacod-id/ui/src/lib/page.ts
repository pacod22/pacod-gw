// What the service filled into the page (crates/pacod-id/src/ui.rs), and small page helpers.

import type { Brand } from '@pacod/ui';

/** The realm's brand: `data-brand` on the root element, set by the service. */
export function brand(): Brand {
	const b = document.documentElement.dataset.brand;
	return b === 'cms' || b === 'kb' || b === 'shop' || b === 'bazaar' ? b : 'gateway';
}

/**
 * A one-time token from the page's address (links in mails), taken out of the address at once:
 * it stays out of the history, of bookmarks and of anything the page loads later.
 */
export function takeToken(name = 'token'): string | null {
	const url = new URL(window.location.href);
	const token = url.searchParams.get(name);
	if (token !== null) {
		url.searchParams.delete(name);
		history.replaceState(history.state, '', url.pathname + url.search + url.hash);
	}
	return token;
}

/** Leaves the page for an address the service gave (an application, or another page). */
export function go(to: string): void {
	window.location.assign(to);
}
