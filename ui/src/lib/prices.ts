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
