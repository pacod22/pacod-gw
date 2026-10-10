/** How limits are worded in tables and state views. */

interface LimitLike {
	amount: number;
	metric: string;
	window: string;
}

const NUMBER = new Intl.NumberFormat('en-GB', { maximumFractionDigits: 4 });

const METRICS: Record<string, string> = {
	input_tokens: 'input tokens',
	output_tokens: 'output tokens',
	cache_read_tokens: 'tokens read from the cache',
	cache_write_tokens: 'tokens written to the cache',
	reasoning_tokens: 'reasoning tokens',
	tokens: 'tokens',
	requests: 'requests',
	// Spend is counted in the reporting currency; without it named, in "currency units".
	spend: 'currency units'
};

const WINDOWS: Record<string, string> = {
	minute: 'per minute',
	hour: 'per hour',
	day: 'per day',
	week: 'per week',
	month: 'per month',
	lifetime: 'in total'
};

/** A quantity of a metric: `1,000 tokens`, `25 USD`; spend in `currency`, the reporting one. */
export function quantity(value: number, metric: string, currency = ''): string {
	const unit = metric === 'spend' && currency ? currency : (METRICS[metric] ?? metric);
	return `${NUMBER.format(value)} ${unit}`;
}

/** "100,000 tokens per day". */
export function limitAmount(limit: LimitLike, currency = ''): string {
	return `${quantity(limit.amount, limit.metric, currency)} ${WINDOWS[limit.window] ?? limit.window}`;
}

/** How much of an allowance is used, 0 to 1 and beyond; 1 when nothing is allowed. */
export function usedShare(used: number, amount: number): number {
	if (amount <= 0) return 1;
	return Math.max(0, used / amount);
}
