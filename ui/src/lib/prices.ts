/** How prices are written in tables. Token rates are per one million tokens. */

const RATE = new Intl.NumberFormat('en-GB', { minimumFractionDigits: 2, maximumFractionDigits: 6 });

/** A rate, or nothing when the provider does not bill it. */
export function rate(value: number | null | undefined): string {
	return typeof value === 'number' ? RATE.format(value) : '';
}

interface PerUnit {
	reasoning?: number | null;
	per_request?: number | null;
	per_character?: number | null;
	per_server_tool_call?: number | null;
	input_audio?: number | null;
	output_audio?: number | null;
	input_image?: number | null;
	output_image?: number | null;
	per_image?: number | null;
	per_audio_second?: number | null;
	per_search_unit?: number | null;
	variants?: Record<string, unknown>;
}

/** The token rates for audio and images, labelled; per million tokens. */
const MEDIA_TOKENS: [keyof PerUnit, string][] = [
	['input_audio', 'Audio input'],
	['output_audio', 'Audio output'],
	['input_image', 'Image input'],
	['output_image', 'Image output']
];

/** The rates per unit, in words, in a fixed order. */
function unitRates(price: PerUnit): string[] {
	const units: [number | null | undefined, string][] = [
		[price.per_request, 'per request'],
		[price.per_character, 'per character'],
		[price.per_server_tool_call, 'per server tool call'],
		[price.per_image, 'per image'],
		[price.per_audio_second, 'per second of audio'],
		[price.per_search_unit, 'per search unit']
	];
	return units.filter(([v]) => typeof v === 'number').map(([v, unit]) => `${rate(v)} ${unit}`);
}

/** The rates that are not per million input or output tokens, in words. */
export function otherRates(price: PerUnit): string {
	const parts: string[] = [];
	if (typeof price.reasoning === 'number') parts.push(`reasoning ${rate(price.reasoning)} / 1M`);
	for (const [key, label] of MEDIA_TOKENS) {
		const value = price[key];
		if (typeof value === 'number') parts.push(`${label.toLowerCase()} ${rate(value)} / 1M`);
	}
	parts.push(...unitRates(price));
	const variants = Object.keys(price.variants ?? {});
	if (variants.length > 0) parts.push(`variants: ${variants.join(', ')}`);
	return parts.join('; ');
}

interface TokenRates extends PerUnit {
	input?: number | null;
	output?: number | null;
	cache_read?: number | null;
	cache_write_5m?: number | null;
	cache_write_1h?: number | null;
}

/** The rates a price has, labelled, in a fixed order; rates it does not bill are left out. */
export function rateList(price: TokenRates): { label: string; value: string }[] {
	const list: { label: string; value: string }[] = [];
	const add = (label: string, value: number | null | undefined) => {
		if (typeof value === 'number') list.push({ label, value: rate(value) });
	};
	add('Input', price.input);
	add('Output', price.output);
	add('Cache read', price.cache_read);
	add('Cache write 5 min', price.cache_write_5m);
	add('Cache write 1 h', price.cache_write_1h);
	add('Reasoning', price.reasoning);
	for (const [key, label] of MEDIA_TOKENS) {
		const value = price[key];
		if (typeof value === 'number') add(label, value);
	}
	for (const text of unitRates(price)) list.push({ label: 'Also', value: text });
	const variants = Object.keys(price.variants ?? {});
	if (variants.length > 0) list.push({ label: 'Variants', value: variants.join(', ') });
	return list;
}

/** The rates as one line of text, for filtering and for screen readers. */
export function rateText(price: TokenRates): string {
	return rateList(price)
		.map(({ label, value }) => `${label} ${value}`)
		.join(', ');
}
