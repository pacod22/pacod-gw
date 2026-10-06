/**
 * The theme for charts drawn on a canvas. A canvas cannot read CSS variables, and the browser
 * gives a variable back as written (`light-dark(…)`), so a colour is resolved through an element
 * that uses it; the chart is drawn again when the theme changes.
 */

/** Resolves colour variables to the colours they stand for in the current theme. */
export function colourResolver(host: HTMLElement): {
	colour: (token: string) => string;
	done: () => void;
} {
	const probe = document.createElement('span');
	probe.hidden = true;
	host.append(probe);
	return {
		colour(token) {
			probe.style.color = `var(${token})`;
			return getComputedStyle(probe).color;
		},
		done: () => probe.remove()
	};
}

/** `rgb(r, g, b)` as the browser computes it, made translucent. */
export function withAlpha(colour: string, alpha: number): string {
	const parts = colour.match(/^rgba?\((\d+),\s*(\d+),\s*(\d+)/);
	return parts ? `rgba(${parts[1]}, ${parts[2]}, ${parts[3]}, ${alpha})` : colour;
}

/** Calls back when the theme changes: chosen in the console or by the system. */
export function onThemeChange(callback: () => void): () => void {
	const observer = new MutationObserver(callback);
	observer.observe(document.documentElement, { attributes: true, attributeFilter: ['data-theme'] });
	const media = matchMedia('(prefers-color-scheme: dark)');
	media.addEventListener('change', callback);
	return () => {
		observer.disconnect();
		media.removeEventListener('change', callback);
	};
}

/** The colours of the axes, the grid and the legend. */
export function axisColours(colour: (token: string) => string) {
	return { muted: colour('--text-muted'), border: colour('--border'), text: colour('--text') };
}
