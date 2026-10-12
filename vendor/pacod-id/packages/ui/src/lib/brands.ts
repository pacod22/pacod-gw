// The brands' names and marks (tokens/brands.json; a test keeps the two equal). Their colours
// and fonts are in tokens.css, chosen with data-brand on the root element.

export type Brand = 'gateway' | 'pacod' | 'vanga';

export const BRANDS: Record<Brand, { monogram: string; wordmark: string }> = {
	gateway: { monogram: 'GW', wordmark: 'PACOD · AI Gateway' },
	pacod: { monogram: 'BG', wordmark: 'PACOD' },
	vanga: { monogram: 'V', wordmark: 'Баба Ванга · Baba Vanga' }
};

/** Up to two initials of a name, or of an email address's local part. */
export function initials(name: string): string {
	const base = name.includes('@') ? name.slice(0, name.indexOf('@')) : name;
	const words = base
		.split(/[\s._-]+/)
		.map((w) => w.trim())
		.filter(Boolean);
	if (words.length === 0) return '?';
	const letters =
		words.length === 1 ? [...words[0]].slice(0, 2) : [[...words[0]][0], [...words[1]][0]];
	return letters.join('').toUpperCase();
}
