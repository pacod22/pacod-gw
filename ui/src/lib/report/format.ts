/** How the values of a report are written, judged by the name of their field. */
import { formatCost, formatDateTime, formatInteger, formatMs } from '../format';
import { humanise } from '../schema/form';

export type Row = Record<string, unknown>;

/** The rows of a report under a key; anything else is no rows. */
export function rowsOf(data: unknown, key: string): Row[] {
	const list = typeof data === 'object' && data !== null ? (data as Row)[key] : undefined;
	return Array.isArray(list)
		? list.filter((r): r is Row => typeof r === 'object' && r !== null)
		: [];
}

export function numberOf(row: Row | null | undefined, key: string): number {
	const value = row?.[key];
	return typeof value === 'number' && Number.isFinite(value) ? value : 0;
}

const PERCENT = new Intl.NumberFormat('en-GB', { style: 'percent', maximumFractionDigits: 1 });
const DECIMAL = new Intl.NumberFormat('en-GB', { maximumFractionDigits: 3 });

/** A value of a report as text. */
export function formatValue(key: string, value: unknown): string {
	if (value === null || value === undefined || value === '') return '';
	if (typeof value === 'boolean') return value ? 'Yes' : 'No';
	if (typeof value === 'number') {
		if (key === 'cost' || key.endsWith('_cost')) return formatCost(value, '');
		if (key.endsWith('_ms')) return formatMs(value);
		if (/(_ratio|_rate|_share)$/.test(key) || key === 'change') return PERCENT.format(value);
		return Number.isInteger(value) ? formatInteger(value) : DECIMAL.format(value);
	}
	if (typeof value === 'string') {
		if (/(_at|^timestamp|^bucket|^from|^to)$/.test(key))
			return formatDateTime(value, key === 'timestamp');
		return value;
	}
	if (Array.isArray(value))
		return value.map((v) => (typeof v === 'object' ? JSON.stringify(v) : String(v))).join(', ');
	return JSON.stringify(value);
}

const LABELS: Record<string, string> = {
	group: 'Group',
	requests: 'Requests',
	errors: 'Errors',
	cost: 'Cost',
	bucket: 'Time (UTC)',
	timestamp: 'Time (UTC)',
	first_request_at: 'First call (UTC)',
	last_request_at: 'Last call (UTC)',
	last_seen_at: 'Last seen (UTC)',
	expires_at: 'Kept until (UTC)',
	gateway_delay_p50_ms: 'Gateway delay, median',
	gateway_delay_p95_ms: 'Gateway delay, 95th percentile',
	gateway_delay_p99_ms: 'Gateway delay, 99th percentile',
	gateway_delay_max_ms: 'Gateway delay, maximum'
};

export function labelOf(key: string): string {
	return LABELS[key] ?? humanise(key);
}

export function isNumeric(rows: Row[], key: string): boolean {
	return rows.some((r) => typeof r[key] === 'number');
}

/** A short label for a bucket of a time series: the hour of a day, or the day. */
export function bucketLabel(value: unknown, bucket: string): string {
	const text = formatDateTime(value);
	return bucket === 'hour' ? text.slice(5) : text.slice(0, 10);
}

/** The values a caller may declare as the complexity of a call. */
export const COMPLEXITIES = ['low', 'medium', 'high'];

/**
 * The choices for grouping a report, with the tags: one choice per declared tag key, or,
 * when the keys are not known, "a tag" whose key is typed next to it.
 */
export function groupOptions(
	plain: string[],
	tagKeys: string[] | null
): { value: string; label: string }[] {
	return [
		...plain.map((value) => ({ value, label: value.replaceAll('_', ' ') })),
		...(tagKeys === null
			? [{ value: 'tag', label: 'a tag (key below)' }]
			: tagKeys.map((key) => ({ value: `tag:${key}`, label: `tag ${key}` })))
	];
}

/** "tag:purpose" as "tag purpose"; other groupings with spaces for underscores. */
export function groupLabel(value: string): string {
	return value.startsWith('tag:') ? `tag ${value.slice(4)}` : value.replaceAll('_', ' ');
}
