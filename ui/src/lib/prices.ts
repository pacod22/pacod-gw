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
	variants?: Record<string, unknown>;
}

/** The rates that are not per million input or output tokens, in words. */
export function otherRates(price: PerUnit): string {
	const parts: string[] = [];
	if (typeof price.reasoning === 'number') parts.push(`reasoning ${rate(price.reasoning)} / 1M`);
	if (typeof price.per_request === 'number') parts.push(`${rate(price.per_request)} per request`);
	if (typeof price.per_character === 'number')
		parts.push(`${rate(price.per_character)} per character`);
	if (typeof price.per_server_tool_call === 'number') {
		parts.push(`${rate(price.per_server_tool_call)} per server tool call`);
	}
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
	const other = [
		typeof price.per_request === 'number' ? `${rate(price.per_request)} per request` : '',
		typeof price.per_character === 'number' ? `${rate(price.per_character)} per character` : '',
		typeof price.per_server_tool_call === 'number'
			? `${rate(price.per_server_tool_call)} per server tool call`
			: ''
	].filter(Boolean);
	for (const text of other) list.push({ label: 'Also', value: text });
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
