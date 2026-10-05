/** Formatting that gives the same text on the server and in the browser: times are UTC. */

/** `2026-10-04 19:33` from an RFC 3339 time. The zone is UTC; column headings say so. */
export function formatDateTime(value: unknown, seconds = false): string {
	if (typeof value !== 'string' || !value) return '';
	const date = new Date(value);
	if (Number.isNaN(date.getTime())) return value;
	const iso = date.toISOString();
	return `${iso.slice(0, 10)} ${iso.slice(11, seconds ? 19 : 16)}`;
}

const INTEGER = new Intl.NumberFormat('en-GB');

export function formatInteger(value: unknown): string {
	return typeof value === 'number' && Number.isFinite(value) ? INTEGER.format(value) : '';
}

/** Milliseconds with a precision that fits the size: `0.42 ms`, `18.3 ms`, `1,240 ms`. */
export function formatMs(value: unknown): string {
	if (typeof value !== 'number' || !Number.isFinite(value)) return '';
	const digits = value < 10 ? 2 : value < 100 ? 1 : 0;
	return `${new Intl.NumberFormat('en-GB', { minimumFractionDigits: digits, maximumFractionDigits: digits }).format(value)} ms`;
}

/** A cost with its currency: small amounts keep their significant digits. */
export function formatCost(value: unknown, currency: unknown): string {
	if (typeof value !== 'number' || !Number.isFinite(value)) return '';
	const digits = value !== 0 && Math.abs(value) < 0.01 ? 6 : 4;
	const amount = new Intl.NumberFormat('en-GB', {
		minimumFractionDigits: 2,
		maximumFractionDigits: digits
	}).format(value);
	return typeof currency === 'string' && currency ? `${amount} ${currency}` : amount;
}
