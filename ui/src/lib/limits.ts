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
	tokens: 'tokens',
	requests: 'requests',
	// The API counts spend in the reporting currency and does not name it here.
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

/** A quantity of a metric: `1,000 tokens`, `25 currency units`. */
export function quantity(value: number, metric: string): string {
	return `${NUMBER.format(value)} ${METRICS[metric] ?? metric}`;
}

/** "100,000 tokens per day". */
export function limitAmount(limit: LimitLike): string {
	return `${quantity(limit.amount, limit.metric)} ${WINDOWS[limit.window] ?? limit.window}`;
}

/** How much of an allowance is used, 0 to 1 and beyond; 1 when nothing is allowed. */
export function usedShare(used: number, amount: number): number {
	if (amount <= 0) return 1;
	return Math.max(0, used / amount);
}
